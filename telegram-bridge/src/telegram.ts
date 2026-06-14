/**
 * Telegram Bot API client with retry logic.
 * Uses native fetch (Node 22+).
 */

import type { TelegramUpdate } from './types.js';

const MAX_RETRIES = 3;
const BASE_DELAY_MS = 1000;
const TIMEOUT_MS = 10_000;
const UPLOAD_TIMEOUT_MS = 60_000;

export class TelegramClient {
  private readonly baseUrl: string;
  private readonly fileBaseUrl: string;

  constructor(botToken: string) {
    this.baseUrl = `https://api.telegram.org/bot${botToken}`;
    this.fileBaseUrl = `https://api.telegram.org/file/bot${botToken}`;
  }

  /**
   * Send a text message to a Telegram chat as plain text.
   *
   * We deliberately omit `parse_mode`: agent output is free-form LLM prose, not
   * authored as Telegram Markdown, so any stray `_ * [ ]` etc. would trigger a
   * 400 "can't parse entities" and the reply would never send. Plain text can
   * never fail to parse. (Trade-off: `**bold**`/`` `code` `` render literally.)
   *
   * Retries up to 3 times with exponential backoff (1s, 2s, 4s) on 5xx or
   * network errors. Respects Retry-After header on 429 responses.
   */
  async sendMessage(chatId: string | number, text: string): Promise<boolean> {
    return this.requestWithRetry('sendMessage', () => ({
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ chat_id: chatId, text }),
      signal: AbortSignal.timeout(TIMEOUT_MS),
    }));
  }

  /**
   * Send a document by URL. Telegram fetches the URL itself (must be publicly
   * reachable, <=20MB). Used by the legacy HTTP `/agent/callback` path.
   */
  async sendDocument(
    chatId: string | number,
    documentUrl: string,
    caption?: string,
  ): Promise<boolean> {
    const payload: Record<string, unknown> = { chat_id: chatId, document: documentUrl };
    if (caption) payload.caption = caption;
    return this.requestWithRetry('sendDocument', () => ({
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(TIMEOUT_MS),
    }));
  }

  /**
   * Upload `file` to a chat as a real multipart document attachment. Unlike
   * {@link sendDocument}, Telegram never fetches a URL — the bridge supplies
   * the bytes, so the source (e.g. a presigned/tailnet URL) only needs to be
   * reachable from the bridge. Telegram's bot upload limit is 50MB.
   */
  async sendDocumentUpload(
    chatId: string | number,
    file: Uint8Array,
    filename: string,
    caption?: string,
  ): Promise<boolean> {
    // Copy into a concrete ArrayBuffer — a Blob part must be ArrayBuffer-backed.
    const buf = new ArrayBuffer(file.byteLength);
    new Uint8Array(buf).set(file);
    return this.requestWithRetry('sendDocument', () => {
      // Rebuilt per attempt: a FormData body is a one-shot stream.
      const form = new FormData();
      form.append('chat_id', String(chatId));
      form.append('document', new Blob([buf]), filename);
      if (caption) form.append('caption', caption);
      // No Content-Type header: fetch sets the multipart boundary itself.
      return { method: 'POST', body: form, signal: AbortSignal.timeout(UPLOAD_TIMEOUT_MS) };
    });
  }

  /**
   * Long-poll for updates. Blocks up to `timeoutSec` server-side until an
   * update is available (or the timeout elapses). `offset` acks all updates
   * with a lower `update_id`. Throws on transport/API error so the poller can
   * back off. Only `message` updates are requested.
   */
  async getUpdates(offset: number, timeoutSec: number): Promise<TelegramUpdate[]> {
    const resp = await fetch(`${this.baseUrl}/getUpdates`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ offset, timeout: timeoutSec, allowed_updates: ['message'] }),
      // Allow the full long-poll window plus slack before aborting.
      signal: AbortSignal.timeout((timeoutSec + 10) * 1000),
    });
    if (!resp.ok) throw new Error(`getUpdates HTTP ${resp.status}`);
    const data = (await resp.json()) as {
      ok: boolean;
      result?: TelegramUpdate[];
      description?: string;
    };
    if (!data.ok) throw new Error(`getUpdates not ok: ${data.description ?? 'unknown'}`);
    return data.result ?? [];
  }

  /**
   * Resolve a `file_id` to its server-side `file_path` via `getFile`. The path
   * is then fetched from the file endpoint by {@link downloadFile}. Returns
   * null on any API/transport error (caller treats the image as unavailable).
   */
  async getFile(fileId: string): Promise<string | null> {
    try {
      const resp = await fetch(`${this.baseUrl}/getFile`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ file_id: fileId }),
        signal: AbortSignal.timeout(TIMEOUT_MS),
      });
      if (!resp.ok) {
        console.warn(`[telegram] getFile HTTP ${resp.status}`);
        return null;
      }
      const data = (await resp.json()) as { ok: boolean; result?: { file_path?: string } };
      return data.ok ? (data.result?.file_path ?? null) : null;
    } catch (err) {
      console.warn(`[telegram] getFile error: ${err instanceof Error ? err.message : String(err)}`);
      return null;
    }
  }

  /**
   * Download a file's bytes from the Telegram file endpoint (the `file_path`
   * comes from {@link getFile}). Returns null on any error.
   */
  async downloadFile(filePath: string): Promise<Uint8Array | null> {
    try {
      const resp = await fetch(`${this.fileBaseUrl}/${filePath}`, {
        signal: AbortSignal.timeout(UPLOAD_TIMEOUT_MS),
      });
      if (!resp.ok) {
        console.warn(`[telegram] downloadFile HTTP ${resp.status}`);
        return null;
      }
      return new Uint8Array(await resp.arrayBuffer());
    } catch (err) {
      console.warn(`[telegram] downloadFile error: ${err instanceof Error ? err.message : String(err)}`);
      return null;
    }
  }

  /**
   * Remove any registered webhook. Required before long-polling: Telegram
   * rejects `getUpdates` while a webhook is set (409 Conflict).
   */
  async deleteWebhook(): Promise<boolean> {
    return this.requestWithRetry('deleteWebhook', () => ({
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ drop_pending_updates: false }),
      signal: AbortSignal.timeout(TIMEOUT_MS),
    }));
  }

  /**
   * POST to a Telegram Bot API method with retry/backoff semantics.
   * `makeInit` is called once per attempt so one-shot bodies (FormData) can be
   * rebuilt. Retries up to 3 times on 5xx/network errors; respects Retry-After
   * on 429.
   */
  private async requestWithRetry(
    method: string,
    makeInit: () => RequestInit,
  ): Promise<boolean> {
    const url = `${this.baseUrl}/${method}`;

    for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
      try {
        const resp = await fetch(url, makeInit());

        if (resp.ok) return true;

        if (resp.status === 429) {
          const retryAfter = parseInt(resp.headers.get('Retry-After') ?? '5', 10);
          console.warn(
            `[telegram] ${method} 429 rate limited, retry-after=${retryAfter}s (attempt ${attempt + 1}/${MAX_RETRIES + 1})`,
          );
          if (attempt < MAX_RETRIES) {
            await sleep(retryAfter * 1000);
            continue;
          }
          return false;
        }

        if (resp.status >= 500) {
          console.warn(
            `[telegram] ${method} returned ${resp.status} (attempt ${attempt + 1}/${MAX_RETRIES + 1})`,
          );
          if (attempt < MAX_RETRIES) {
            await sleep(BASE_DELAY_MS * Math.pow(2, attempt));
            continue;
          }
          return false;
        }

        const errText = await resp.text().catch(() => '');
        console.error(
          `[telegram] ${method} failed with ${resp.status}: ${errText}`,
        );
        return false;
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        console.warn(
          `[telegram] ${method} error: ${msg} (attempt ${attempt + 1}/${MAX_RETRIES + 1})`,
        );
        if (attempt < MAX_RETRIES) {
          await sleep(BASE_DELAY_MS * Math.pow(2, attempt));
          continue;
        }
        return false;
      }
    }

    return false;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
