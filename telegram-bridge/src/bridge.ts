/**
 * BridgeService — orchestrates the Telegram ↔ Dimension message flow.
 *
 * Handles:
 * - Incoming Telegram updates → POST /run, dispatch or queue
 * - SSE run events (bundle messages, state changes) → forward to Telegram,
 *   complete invocations, flush queued messages
 * - Session-aware message buffering during in-flight invocations
 */
import type { BridgeDb } from './db.js';
import type { TelegramClient } from './telegram.js';
import type { DimensionClient } from './dimension.js';
import { subscribeRunEvents } from './dimension.js';
import type {
  TelegramUpdate,
  TelegramPhotoSize,
  AgentCallbackEvent,
  BridgeConfig,
  InboundImage,
} from './types.js';
import { MAX_PHOTO_WIDTH } from './types.js';

/** Max document size we'll relay to Telegram (its bot upload limit is 50MB). */
const MAX_DOC_BYTES = 50 * 1024 * 1024;
/** Max inbound photo size we'll pull from Telegram and embed in a run payload. */
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
/** Timeout for downloading a bundle artifact before uploading to Telegram. */
const DOC_DOWNLOAD_TIMEOUT_MS = 30_000;

export class BridgeService {
  private readonly db: BridgeDb;
  private readonly telegram: TelegramClient;
  private readonly dimension: DimensionClient;
  private readonly apiUrl: string;
  private readonly apiKey: string;

  constructor(
    db: BridgeDb,
    telegramClient: TelegramClient,
    dimensionClient: DimensionClient,
    config: BridgeConfig,
  ) {
    this.db = db;
    this.telegram = telegramClient;
    this.dimension = dimensionClient;
    this.apiUrl = config.dimensionApiUrl;
    this.apiKey = config.dimensionApiKey;
  }

  /**
   * Handle an incoming Telegram webhook update.
   * Either dispatches a new invocation or queues the message if one is in-flight.
   */
  async handleTelegramUpdate(update: TelegramUpdate): Promise<void> {
    const message = update.message;
    if (!message?.chat?.id) {
      console.log('[bridge] Ignoring update with no chat');
      return;
    }

    const chatId = String(message.chat.id);
    // A photo message carries its text in `caption`, not `text`.
    const text = message.text ?? message.caption ?? '';
    const hasPhoto = Array.isArray(message.photo) && message.photo.length > 0;

    if (!text && !hasPhoto) {
      console.log('[bridge] Ignoring update with no text or photo');
      return;
    }

    // Slash commands come only from real text messages (never a photo caption).
    if (message.text) {
      const command = parseSlashCommand(message.text);
      if (command === '/new' || command === '/reset') {
        const session = this.db.resetSession(chatId);
        console.log(
          `[bridge] Session reset for chat=${chatId} → new session=${session.session_id}`,
        );
        await this.telegram.sendMessage(
          chatId,
          '🆕 Started a fresh session — your previous conversation and issue list are cleared. Send a message to begin.',
        );
        return;
      }
    }

    // Pull the photo bytes (if any) before dispatch.
    let images: InboundImage[] = [];
    if (hasPhoto) {
      const image = await this.downloadTelegramPhoto(message.photo!);
      if (image) {
        images = [image];
      } else {
        console.warn(`[bridge] photo download failed for chat=${chatId}`);
        await this.telegram.sendMessage(
          chatId,
          "⚠️ Sorry, I couldn't download that image. Please try sending it again.",
        );
        return;
      }
    }

    const session = this.db.getOrCreateSession(chatId);
    const inFlight = this.db.getInFlightInvocation(session.session_id);

    if (inFlight) {
      // The message queue is text-only, so a photo arriving mid-run can't be
      // buffered — ask the user to resend it once the current run replies.
      if (hasPhoto) {
        console.warn(
          `[bridge] photo arrived mid-run for session=${session.session_id}; asking user to resend`,
        );
        await this.telegram.sendMessage(
          chatId,
          "⏳ Still finishing your previous message — please resend the photo after I reply.",
        );
        return;
      }
      this.db.queueMessage(session.session_id, text);
      console.log(
        `[bridge] Message queued for session=${session.session_id} (in-flight invocation=${inFlight.invocation_id})`,
      );
      return;
    }

    // No in-flight invocation — dispatch immediately (with any image).
    await this.dispatchWithTracking(session.session_id, text, images);
  }

  /**
   * Download the best-fit variant of a Telegram photo and return it as a
   * base64 image ready for the run payload. Picks the largest size at or under
   * {@link MAX_PHOTO_WIDTH} to bound vision-token and payload cost. Telegram
   * photos are always JPEG. Returns null on any failure.
   */
  private async downloadTelegramPhoto(
    sizes: TelegramPhotoSize[],
  ): Promise<InboundImage | null> {
    const choice = pickPhotoSize(sizes);
    if (!choice) return null;

    const filePath = await this.telegram.getFile(choice.file_id);
    if (!filePath) return null;

    const bytes = await this.telegram.downloadFile(filePath);
    if (!bytes) return null;
    if (bytes.byteLength > MAX_IMAGE_BYTES) {
      console.warn(`[bridge] photo too large (${bytes.byteLength} bytes), skipping`);
      return null;
    }

    return {
      data: Buffer.from(bytes).toString('base64'),
      mimeType: 'image/jpeg',
      filename: filePath.split('/').pop() || 'photo.jpg',
    };
  }

  /**
   * Handle an agent event delivered via the run's SSE stream (the body of a
   * `bundle` event). Forwards Message/Document events to Telegram and
   * flushes any queued messages.
   */
  async handleAgentCallback(event: AgentCallbackEvent): Promise<void> {
    // Validate event has required fields
    if (!event.session_id || !event.event_type) {
      console.warn('[bridge] Ignoring callback with missing session_id or event_type');
      return;
    }

    // Forward Message and Document events to Telegram; tool calls/results are informational
    if (event.event_type !== 'Message' && event.event_type !== 'Document') {
      console.log(
        `[bridge] Ignoring non-forwardable event_type=${event.event_type} for session=${event.session_id}`,
      );
      return;
    }

    // Look up session by session_id to get the chat_id
    const session = this.db.getSessionBySessionId(event.session_id);
    if (!session) {
      console.warn(
        `[bridge] Unknown session_id=${event.session_id}, ignoring callback`,
      );
      return;
    }

    if (event.event_type === 'Document') {
      await this.handleDocumentEvent(session.chat_id, event);
      console.log(
        `[bridge] Document forwarded for session=${session.session_id}`,
      );
      // Document events don't complete the invocation — the agent still sends
      // a final Message event after, which completes it and flushes the queue.
      return;
    }

    // Send the agent's response to Telegram
    const responseText = event.content?.content;
    if (responseText) {
      await this.telegram.sendMessage(session.chat_id, responseText);
    }

    // Complete the in-flight invocation
    const inFlight = this.db.getInFlightInvocation(session.session_id);
    if (inFlight) {
      this.db.completeInvocation(inFlight.invocation_id);
    }

    // Flush queued messages: concatenate and dispatch as a single batch
    const queued = this.db.getQueuedMessages(session.session_id);
    if (queued.length > 0) {
      const batchedText = queued.map((m) => m.text).join('\n');
      this.db.clearQueue(session.session_id);
      await this.dispatchWithTracking(session.session_id, batchedText);
      console.log(
        `[bridge] Flushed ${queued.length} queued message(s) for session=${session.session_id}`,
      );
    }

    console.log(
      `[bridge] Callback processed for session=${session.session_id}`,
    );
  }

  /**
   * Forward a Document event to Telegram via sendDocument.
   * The content.content field is a JSON string of { url, filename?, caption? }.
   * Telegram fetches the URL itself, so it must be publicly reachable (e.g.
   * a presigned S3 URL) and <=20MB.
   */
  private async handleDocumentEvent(
    chatId: string,
    event: AgentCallbackEvent,
  ): Promise<void> {
    const raw = event.content?.content;
    if (!raw) {
      console.warn(
        `[bridge] Document event with empty content for session=${event.session_id}`,
      );
      return;
    }

    let parsed: { url?: string; caption?: string };
    try {
      parsed = JSON.parse(raw) as { url?: string; caption?: string };
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.warn(
        `[bridge] Document event content was not valid JSON for session=${event.session_id}: ${msg}`,
      );
      return;
    }

    if (!parsed.url) {
      console.warn(
        `[bridge] Document event missing 'url' for session=${event.session_id}`,
      );
      return;
    }

    await this.telegram.sendDocument(chatId, parsed.url, parsed.caption);
  }

  /**
   * Dispatch a run invocation and track it as in-flight, then subscribe to
   * the SSE event stream for that run. The bundle emits chat as `kind =
   * "message"` events (and `tool_call`/`tool_result` noise); the worker emits
   * `kind = "state"` lifecycle events. See `streamRunEvents` for routing.
   */
  private async dispatchWithTracking(
    sessionId: string,
    text: string,
    images: InboundImage[] = [],
  ): Promise<void> {
    let invocationId: string;
    let eventsUrl: string | undefined;
    try {
      const result = await this.dimension.dispatchRun(sessionId, text, images);
      invocationId = result.invocation_id;
      eventsUrl = result.events_url;
      this.db.setInvocationInFlight(sessionId, invocationId);
      console.log(
        `[bridge] Invocation dispatched: invocation_id=${invocationId} session=${sessionId}`,
      );
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error(`[bridge] Dispatch failed for session=${sessionId}: ${msg}`);
      const session = this.db.getSessionBySessionId(sessionId);
      if (session) {
        await this.telegram.sendMessage(
          session.chat_id,
          '⚠️ Sorry, I was unable to process your message. Please try again later.',
        );
      }
      return;
    }

    if (!eventsUrl) {
      console.warn(
        `[bridge] No events_url in run response for invocation=${invocationId}; agent events will not be received`,
      );
      return;
    }

    // Fire-and-forget the SSE consumer. If the stream dies the invocation is
    // marked failed so the session doesn't stay stuck in-flight forever, and
    // any queued messages are flushed as a fresh dispatch.
    void this.streamRunEvents(sessionId, invocationId, eventsUrl).catch(
      async (err) => {
        const msg = err instanceof Error ? err.message : String(err);
        console.error(
          `[bridge] SSE stream for session=${sessionId} ended with error: ${msg}`,
        );
        try {
          this.db.failInvocation(invocationId);
          const queued = this.db.getQueuedMessages(sessionId);
          if (queued.length > 0) {
            const batchedText = queued.map((m) => m.text).join('\n');
            this.db.clearQueue(sessionId);
            await this.dispatchWithTracking(sessionId, batchedText);
          }
        } catch (recoverErr) {
          const rmsg =
            recoverErr instanceof Error ? recoverErr.message : String(recoverErr);
          console.error(
            `[bridge] Failed to recover after SSE error for session=${sessionId}: ${rmsg}`,
          );
        }
      },
    );
  }

  /**
   * Consume the SSE stream for a single run. We forward only the run's final
   * message — the `stdout-final` event, which carries the bundle's final
   * stdout (every bundle writes its answer there). Everything else on the
   * stream is non-chat and ignored:
   *
   *   - "stdout-final" → the final reply; forwarded to Telegram.
   *   - "document"     → bundle artifact, body `{ url, filename?, caption? }`.
   *                      The bridge downloads the URL and uploads the bytes as
   *                      a real Telegram file attachment.
   *   - "state"        → lifecycle; `phase` completed/process_exited finalises
   *                      the invocation and flushes any queued messages.
   *   - "message" / "tool_call" / "tool_result" / "stderr" → ignored.
   *
   * The gateway closes the stream after the terminal `state` event, so the
   * loop ends on its own.
   */
  private async streamRunEvents(
    sessionId: string,
    invocationId: string,
    eventsUrl: string,
  ): Promise<void> {
    const session = this.db.getSessionBySessionId(sessionId);
    if (!session) {
      console.warn(
        `[bridge] streamRunEvents: unknown session=${sessionId}, dropping stream`,
      );
      return;
    }
    const chatId = session.chat_id;
    let finalized = false;

    const finalize = async (): Promise<void> => {
      if (finalized) return;
      finalized = true;
      this.db.completeInvocation(invocationId);
      const queued = this.db.getQueuedMessages(sessionId);
      if (queued.length > 0) {
        const batchedText = queued.map((m) => m.text).join('\n');
        this.db.clearQueue(sessionId);
        await this.dispatchWithTracking(sessionId, batchedText);
        console.log(
          `[bridge] Flushed ${queued.length} queued message(s) for session=${sessionId}`,
        );
      }
    };

    for await (const ev of subscribeRunEvents(this.apiUrl, this.apiKey, eventsUrl)) {
      const body = ev.data.body as unknown;
      switch (ev.kind) {
        case 'stdout-final': {
          // The bundle's final answer (stdout-final/stderr arrive as strings).
          if (typeof body === 'string') {
            const text = body.trim();
            if (text) await this.telegram.sendMessage(chatId, text);
          }
          break;
        }
        case 'document': {
          await this.forwardDocument(
            chatId,
            body as { url?: string; filename?: string; caption?: string } | undefined,
          );
          break;
        }
        case 'state': {
          const phase = (body as { phase?: string } | undefined)?.phase;
          if (phase === 'completed' || phase === 'process_exited') {
            await finalize();
          }
          break;
        }
        default:
          // message, tool_call, tool_result, stderr: not the final reply.
          break;
      }
    }

    // Stream closed. Finalise even if no terminal state arrived (e.g. the run
    // crashed before emitting `completed`) so the invocation never wedges.
    await finalize();
  }

  /**
   * Relay a bundle-produced artifact to Telegram as a real file attachment.
   * The bundle sends a (presigned) `url`; the bridge downloads it and uploads
   * the bytes itself, so the URL only needs to be reachable from the bridge —
   * Telegram never sees it. Failures are logged and swallowed.
   */
  private async forwardDocument(
    chatId: string,
    doc: { url?: string; filename?: string; caption?: string } | undefined,
  ): Promise<void> {
    if (!doc?.url) {
      console.warn(`[bridge] document event missing url for chat=${chatId}`);
      return;
    }
    try {
      const resp = await fetch(doc.url, {
        signal: AbortSignal.timeout(DOC_DOWNLOAD_TIMEOUT_MS),
      });
      if (!resp.ok) {
        console.warn(`[bridge] document download failed: HTTP ${resp.status}`);
        return;
      }
      const bytes = new Uint8Array(await resp.arrayBuffer());
      if (bytes.byteLength > MAX_DOC_BYTES) {
        console.warn(
          `[bridge] document too large (${bytes.byteLength} bytes > ${MAX_DOC_BYTES}), skipping`,
        );
        return;
      }
      const filename = doc.filename ?? 'document';
      await this.telegram.sendDocumentUpload(chatId, bytes, filename, doc.caption);
      console.log(
        `[bridge] document forwarded (${bytes.byteLength} bytes, ${filename}) to chat=${chatId}`,
      );
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.warn(`[bridge] document forward failed: ${msg}`);
    }
  }
}

/**
 * Extract a leading Telegram bot command from message text, normalised to
 * lowercase with any `@botname` suffix stripped (e.g. `/New@MyBot foo` →
 * `/new`). Returns null when the text doesn't start with a command.
 */
function parseSlashCommand(text: string): string | null {
  const token = text.trim().split(/\s+/, 1)[0];
  if (!token.startsWith('/')) return null;
  return token.split('@', 1)[0].toLowerCase();
}

/**
 * Choose which Telegram photo variant to download. The `photo` array is
 * ascending by resolution; we take the largest variant at or under
 * {@link MAX_PHOTO_WIDTH}px, falling back to the smallest if all exceed it.
 * Keeps vision-token cost and payload size bounded while staying legible.
 */
function pickPhotoSize(sizes: TelegramPhotoSize[]): TelegramPhotoSize | undefined {
  if (sizes.length === 0) return undefined;
  const sorted = [...sizes].sort((a, b) => a.width - b.width);
  const underCap = sorted.filter((s) => s.width <= MAX_PHOTO_WIDTH);
  return underCap.length > 0 ? underCap[underCap.length - 1] : sorted[0];
}
