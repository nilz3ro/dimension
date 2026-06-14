/**
 * Dimension gateway client for dispatching async POST /run invocations.
 * Uses native fetch (Node 22+).
 */
import type {
  DimensionContent,
  DimensionRunRequest,
  DimensionRunResponse,
  InboundImage,
} from './types.js';

const MAX_RETRIES = 3;
const BASE_DELAY_MS = 1000;
const TIMEOUT_MS = 30_000;

/**
 * Upper bound on a single SSE stream's lifetime. Runs are capped server-side
 * (bundle timeout_secs, max 300s) and the gateway closes the stream ~60s
 * after completion, so a stream alive past this is hung — abort it so the
 * caller's error path can fail the invocation instead of waiting forever.
 */
const SSE_MAX_STREAM_MS = 15 * 60_000;

export class DimensionClient {
  private readonly apiUrl: string;
  private readonly apiKey: string;
  private readonly bundleId: string;

  constructor(apiUrl: string, apiKey: string, bundleId: string) {
    this.apiUrl = apiUrl;
    this.apiKey = apiKey;
    this.bundleId = bundleId;
  }

  /**
   * Dispatch an async POST /run invocation to the Dimension gateway.
   * Retries up to 3 times with exponential backoff (1s, 2s, 4s) on 5xx or
   * network errors.
   *
   * @returns The { invocation_id, status } from the 202 response
   * @throws Error if all retries exhausted or non-retryable status
   */
  async dispatchRun(
    sessionId: string,
    messageText: string,
    images: InboundImage[] = [],
  ): Promise<DimensionRunResponse> {
    const url = `${this.apiUrl}/run`;

    // Always include a text block (possibly empty) followed by any image
    // blocks. Image blocks carry base64 `data` + `mimeType`; the payload is
    // opaque pass-through, so they reach the bundle's stdin intact.
    const content: DimensionContent[] = [{ type: 'text', text: messageText }];
    for (const img of images) {
      content.push({ type: 'image', data: img.data, mimeType: img.mimeType, filename: img.filename });
    }

    const requestBody: DimensionRunRequest = {
      bundle_id: this.bundleId,
      mode: 'async',
      payload: {
        role: 'user',
        content,
        session_id: sessionId,
        bundle_id: this.bundleId,
        history: [],
        truncation: {
          total_messages: 0,
          included_messages: 0,
          truncated: false,
        },
      },
    };

    const body = JSON.stringify(requestBody);

    for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
      try {
        const resp = await fetch(url, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            Authorization: `Bearer ${this.apiKey}`,
          },
          body,
          signal: AbortSignal.timeout(TIMEOUT_MS),
        });

        if (resp.ok) {
          const data = (await resp.json()) as DimensionRunResponse;
          if (!data.invocation_id) {
            console.error('[dimension] Malformed response: missing invocation_id');
            throw new Error('Malformed response from Dimension gateway');
          }
          return data;
        }

        // Retry on 5xx
        if (resp.status >= 500) {
          console.warn(
            `[dimension] POST /run returned ${resp.status} (attempt ${attempt + 1}/${MAX_RETRIES + 1})`,
          );
          if (attempt < MAX_RETRIES) {
            await sleep(BASE_DELAY_MS * Math.pow(2, attempt));
            continue;
          }
          throw new Error(
            `Dimension gateway returned ${resp.status} after ${MAX_RETRIES + 1} attempts`,
          );
        }

        // Non-retryable client error
        const errText = await resp.text().catch(() => '');
        console.error(
          `[dimension] POST /run failed with ${resp.status}: ${errText}`,
        );
        throw new Error(`Dimension gateway returned ${resp.status}: ${errText}`);
      } catch (err) {
        if (
          err instanceof Error &&
          err.message.startsWith('Dimension gateway')
        ) {
          throw err;
        }

        // Network error or timeout
        const msg = err instanceof Error ? err.message : String(err);
        console.warn(
          `[dimension] POST /run error: ${msg} (attempt ${attempt + 1}/${MAX_RETRIES + 1})`,
        );
        if (attempt < MAX_RETRIES) {
          await sleep(BASE_DELAY_MS * Math.pow(2, attempt));
          continue;
        }
        throw new Error(
          `Dimension gateway unreachable after ${MAX_RETRIES + 1} attempts: ${msg}`,
        );
      }
    }

    // Should not reach here
    throw new Error('Dimension dispatch exhausted retries');
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** One parsed `event:`/`data:` block from an SSE stream. */
export interface RunEvent {
  /** Sequence number from the `id:` line (parsed). */
  seq: number;
  /** Event name from the `event:` line. Common values: "bundle", "state". */
  kind: string;
  /** Parsed JSON body from the `data:` line. */
  data: Record<string, unknown>;
}

/**
 * Subscribe to a run's event stream. Yields each parsed SSE frame until the
 * stream closes — the server ends the stream right after the terminal
 * `state` event (`body.phase === "completed"`). Callers filter by `kind`
 * and reach into `data.body` (a parsed object for JSON events) for the payload.
 *
 * `Last-Event-ID` is sent automatically on reconnects driven by the caller.
 */
export async function* subscribeRunEvents(
  apiUrl: string,
  apiKey: string,
  eventsUrl: string,
  options: { lastEventId?: number } = {},
): AsyncGenerator<RunEvent, void, unknown> {
  const url = new URL(eventsUrl, apiUrl).toString();
  const headers: Record<string, string> = {
    Authorization: `Bearer ${apiKey}`,
    Accept: 'text/event-stream',
  };
  if (options.lastEventId !== undefined) {
    headers['Last-Event-ID'] = String(options.lastEventId);
  }
  const resp = await fetch(url, {
    method: 'GET',
    headers,
    signal: AbortSignal.timeout(SSE_MAX_STREAM_MS),
  });
  if (!resp.ok || !resp.body) {
    throw new Error(`SSE subscribe failed: HTTP ${resp.status}`);
  }
  const reader = resp.body.getReader();
  const decoder = new TextDecoder();
  let buf = '';
  while (true) {
    const { done, value } = await reader.read();
    if (done) return;
    buf += decoder.decode(value, { stream: true });
    let idx: number;
    while ((idx = buf.indexOf('\n\n')) !== -1) {
      const frame = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      const ev = parseSseFrame(frame);
      if (ev) yield ev;
    }
  }
}

function parseSseFrame(frame: string): RunEvent | null {
  let kind = 'message';
  let id = 0;
  const dataLines: string[] = [];
  for (const line of frame.split('\n')) {
    if (line.startsWith(':')) continue; // comment / heartbeat
    if (line.startsWith('event:')) kind = line.slice(6).trim();
    else if (line.startsWith('id:')) id = Number(line.slice(3).trim()) || 0;
    else if (line.startsWith('data:')) dataLines.push(line.slice(5).trimStart());
  }
  if (dataLines.length === 0) return null;
  const raw = dataLines.join('\n');
  let data: Record<string, unknown> = {};
  try {
    data = JSON.parse(raw) as Record<string, unknown>;
  } catch {
    data = { body: raw };
  }
  return { seq: id, kind, data };
}
