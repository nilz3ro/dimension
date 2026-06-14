// ── Telegram types ────────────────────────────────────────────────────────────

export interface TelegramUser {
  id: number;
  first_name: string;
}

export interface TelegramChat {
  id: number;
}

/** One size variant of a Telegram photo (the `photo` array is ascending by size). */
export interface TelegramPhotoSize {
  file_id: string;
  file_unique_id: string;
  width: number;
  height: number;
  file_size?: number;
}

export interface TelegramMessage {
  message_id: number;
  chat: TelegramChat;
  text?: string;
  /** Caption accompanying a photo/media message. */
  caption?: string;
  /** Present on photo messages; ascending by resolution. */
  photo?: TelegramPhotoSize[];
  from?: TelegramUser;
}

/** Incoming Telegram webhook update. */
export interface TelegramUpdate {
  update_id: number;
  message?: TelegramMessage;
}

// ── Agent event types (body of `bundle` SSE events) ──────────────────────────

export interface AgentCallbackContent {
  role: string;
  content: string;
}

/**
 * Event emitted by a bundle via `dimension.sendMessage(...)` /
 * `dimension.send(...)` and delivered to the bridge inside a `bundle` SSE
 * event. Shape matches `sendMessage` in
 * `agents/packages/shared/src/events.ts`.
 */
export interface AgentCallbackEvent {
  session_id: string;
  // "Message"   — final assistant text; forwarded to Telegram as a text message
  // "Document"  — content.content is JSON { url, filename?, caption? }; forwarded via Telegram sendDocument
  // "ToolCall"  — informational; logged but not forwarded
  // "ToolResult"— informational; logged but not forwarded
  event_type: string;
  event_id: string;
  content: AgentCallbackContent;
  timestamp: string;
}

// ── Dimension gateway types (matches POST /run in run.rs) ────────────────────

export interface DimensionContent {
  type: string;
  text?: string;
  /** base64-encoded bytes, present on `type: "image"` blocks. */
  data?: string;
  /** MIME type of an image block, e.g. "image/jpeg". */
  mimeType?: string;
  filename?: string;
}

/** An image pulled from Telegram, ready to embed in a run payload. */
export interface InboundImage {
  /** base64-encoded image bytes. */
  data: string;
  mimeType: string;
  filename: string;
}

/** Photo `PhotoSize.width` ceiling when choosing which variant to download.
 *  Keeps vision token cost and payload size bounded while staying legible. */
export const MAX_PHOTO_WIDTH = 1024;

export interface HistoryEntry {
  role: string;
  content: string;
  timestamp: string;
}

export interface DimensionTruncation {
  total_messages: number;
  included_messages: number;
  truncated: boolean;
}

/**
 * The inner payload forwarded to the VM.
 * Matches `DimensionRequest` in `agents/packages/shared/src/agent.ts`.
 *
 * Note: the legacy `webhook_url` field was removed. Bundles now emit events
 * by calling `dimension.send(...)` from `@dimension-agents/shared`, and the
 * bridge consumes them via `GET /runs/:id/events` (SSE).
 */
export interface DimensionPayload {
  role: string;
  content: DimensionContent[];
  session_id: string;
  bundle_id: string;
  history: HistoryEntry[];
  truncation: DimensionTruncation;
}

/**
 * Top-level POST /run request body.
 * Matches `RunRequest` in `crates/dimension-gateway/src/handlers/run.rs`.
 */
export interface DimensionRunRequest {
  bundle_id: string;
  mode: string; // "async" for bridge
  payload: DimensionPayload;
}

/** Async 202 response from POST /run. */
export interface DimensionRunResponse {
  invocation_id: string;
  status: string;
  /** Path (relative to the gateway base URL) of the SSE event stream for
   *  this run. Subscribe with `GET {apiUrl}{events_url}` and an `Authorization:
   *  Bearer ...` header to receive `state` and `bundle` events. */
  events_url: string;
}

// ── Persistence models ───────────────────────────────────────────────────────

export interface Session {
  chat_id: string;
  session_id: string;
  created_at: string;
}

export interface QueuedMessage {
  id: number;
  session_id: string;
  text: string;
  received_at: string;
}

export interface Invocation {
  session_id: string;
  invocation_id: string;
  status: string; // "in_flight" | "completed" | "failed"
  created_at: string;
}

// ── Bridge config ────────────────────────────────────────────────────────────

export interface BridgeConfig {
  telegramBotToken: string;
  dimensionApiUrl: string;
  dimensionApiKey: string;
  dimensionBundleId: string;
  port: number;
  /** When true, long-poll Telegram for updates instead of using a webhook. */
  polling?: boolean;
  dbPath?: string;
}
