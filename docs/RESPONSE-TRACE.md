# Response Trace: Agent stdout → Telegram Message

This document traces every hop a response takes from an agent's stdout inside a Firecracker microVM to a Telegram message delivered to the user. It covers both the single-host (vsock direct) and multi-host (gRPC proxy) paths, identifies what data is available and lost at each stage, and proposes where structured response handling could be injected.

---

## Architecture Overview

```
┌─────────────────────┐
│  Agent Process       │  (inside Firecracker VM)
│  stdout / stderr     │
└──────────┬──────────┘
           │ line-buffered reads
           ▼
┌─────────────────────┐
│  dimension-agent     │  (guest agent binary, also inside VM)
│  child.rs            │
└──────────┬──────────┘
           │ protobuf Envelope frames over vsock
           ▼
┌─────────────────────────────────────────────────────┐
│  dimension-gateway                                   │
│  ┌─────────────────┐   ┌──────────────────────────┐ │
│  │ orchestration/   │──▶│ BackendEvent (mpsc chan)  │ │
│  │ handler.rs       │   └──────────┬───────────────┘ │
│  └─────────────────┘              │                  │
│                     ┌─────────────┼─────────────┐    │
│                     ▼             ▼             ▼    │
│              ┌──────────┐  ┌──────────┐  ┌─────────┐│
│              │ session/ │  │ sse/     │  │channels/││
│              │handler.rs│  │stream.rs │  │bridge.rs││
│              └──────────┘  └──────────┘  └─────────┘│
│                     │             │             │    │
│                     ▼             ▼             ▼    │
│              ┌──────────┐  ┌──────────┐  ┌─────────┐│
│              │ SQLite   │  │ SSE to   │  │Telegram ││
│              │ store    │  │ HTTP cli │  │Bot API  ││
│              └──────────┘  └──────────┘  └─────────┘│
└─────────────────────────────────────────────────────┘
```

---

## Hop-by-Hop Trace

### Hop 1: Agent Process → Guest Agent (child.rs)

**File:** `dimension-agent/src/child.rs`

**What happens:**
- The guest agent spawns the configured binary (`config.binary_path`) as a child process
- The `MessageRequest` JSON payload is written to the child's **stdin**, then stdin is closed (EOF)
- **stdout** and **stderr** are read **line-by-line** via `BufReader::lines()`
- Each line becomes a `DataChunk` protobuf message inside an `Envelope`
- Lines are tagged with `content_type`: `"stdout"` or `"stderr"`
- A global `sequence: u32` counter increments across both streams (interleaved ordering)
- After both streams close, the child's exit code is captured

**Data available:**
| Field | Value |
|-------|-------|
| `request_id` | UUID string (set by caller) |
| `content` | One line of text (bytes) |
| `content_type` | `"stdout"` or `"stderr"` |
| `sequence` | Monotonic u32 counter |

**What gets lost/flattened:**
- ⚠️ **Line buffering destroys streaming granularity.** If the agent writes partial lines or uses chunked output (e.g., streaming tokens without newlines), nothing arrives until `\n`. This is the **first major flattening point**.
- ⚠️ **All output is treated as opaque text.** If the agent emits structured JSON, tool calls, or metadata, it's all just bytes in `DataChunk.content`. There's no semantic parsing.
- ⚠️ **No mechanism for the child to send Done metadata.** The `Done` envelope is synthesized by the guest agent after the child exits, not by the child itself. The child cannot signal "here's my token usage" or "here are my artifacts."
- ⚠️ **Binary output is impossible.** `BufReader::lines()` splits on `\n` and returns Strings, so binary data would be corrupted.

**Injection point for structured responses:**
> The agent could write a structured protocol to stdout (e.g., JSON-lines with a `type` field). `child.rs` would need to be extended to parse these and map them to different `Envelope` payload types instead of always using `DataChunk`. Alternatively, a new `content_type` value (e.g., `"structured"` or `"json"`) could signal that the content should be parsed downstream.

---

### Hop 2: Guest Agent → Host Gateway (vsock transport)

**File:** `dimension-protocol/proto/dimension.proto`, `dimension-protocol/src/codec.rs`

**What happens:**
- The `Envelope` is serialized using **prost** (protobuf) with **varint length-delimited framing**
- Sent over a **vsock** connection (VM-host socket, port 1024)
- `ProtocolCodec` handles encode/decode with a 4 MiB max message size
- The host reads frames using `tokio_util::codec::Framed`

**Wire protocol:**
```protobuf
message Envelope {
  string request_id = 1;
  oneof payload {
    Request request = 10;     // host→guest only
    DataChunk data_chunk = 11; // guest→host streaming
    Error error = 12;          // either direction
    Done done = 13;            // guest→host completion
  }
}

message DataChunk {
  bytes content = 1;
  uint32 sequence = 2;
  optional string content_type = 3;
}

message Done {
  map<string, string> metadata = 1;
}
```

**Data available:** Full protobuf fidelity — `request_id`, `content`, `sequence`, `content_type`, `metadata` map.

**What gets lost/flattened:** Nothing at this hop — the protobuf is a faithful representation.

**Injection point:**
> The protobuf schema could be extended with new payload types:
> ```protobuf
> message ToolCall {
>   string tool_name = 1;
>   string arguments_json = 2;
>   string call_id = 3;
> }
> message Artifact {
>   string name = 1;
>   bytes content = 2;
>   string mime_type = 3;
> }
> message Metadata {
>   map<string, string> data = 1;
> }
> ```
> These would be added to the `Envelope.payload` oneof.

---

### Hop 3: VmOrchestrationHandler — Envelope → BackendEvent

**File:** `dimension-gateway/src/orchestration/handler.rs`

**What happens:**
- `process_envelope()` matches on `Envelope.payload`:
  - `DataChunk` with `content_type == "ready"` → **filtered out** (internal handshake)
  - `DataChunk` with `content_type == "stderr"` → either **suppressed** (logged server-side only) or forwarded as `BackendEvent::Message`, depending on `suppress_guest_stderr` config
  - All other `DataChunk` → `BackendEvent::Message { content: String }`
  - `Done` → `BackendEvent::Done { duration_ms, bytes_streamed, exit }`
  - `Error` → `BackendEvent::Error { code, message, details }`
  - `Request` / empty → logged and ignored

**Data available at output (`BackendEvent` enum):**
```rust
enum BackendEvent {
    Message { content: String },
    Status { status: String },
    Error { code: String, message: String, details: Option<Value> },
    Done { duration_ms: u64, bytes_streamed: u64, exit: String },
}
```

**What gets lost/flattened:**
- ⚠️ **`sequence` number is dropped.** `BackendEvent::Message` has no sequence field. Out-of-order delivery is impossible to detect downstream.
- ⚠️ **`content_type` is dropped** (except for stderr filtering). After this hop, there's no way to distinguish stdout from stderr, or text from JSON from binary. Everything is `Message { content: String }`.
- ⚠️ **`request_id` is dropped** from individual events. It's tracked at the connection level but not propagated per-event.
- ⚠️ **`Done.metadata` is mostly dropped.** Only `metadata["exit"]` is extracted; all other metadata keys are lost.
- ⚠️ **This is the critical flattening point.** The rich protobuf `Envelope` is collapsed into a simple 4-variant enum. Any structured data the agent sent is now just a `String`.

**Injection point:**
> `BackendEvent` should be extended with new variants:
> ```rust
> enum BackendEvent {
>     Message { content: String },
>     ToolCall { tool_name: String, arguments: Value, call_id: String },
>     Artifact { name: String, content: Vec<u8>, mime_type: String },
>     Metadata { data: HashMap<String, String> },
>     Status { status: String },
>     Error { code: String, message: String, details: Option<Value> },
>     Done { duration_ms: u64, bytes_streamed: u64, exit: String, metadata: HashMap<String, String> },
> }
> ```
> `process_envelope()` would match new protobuf payload types to these variants.

---

### Hop 4a: BackendEvent → SSE Stream (API clients)

**File:** `dimension-gateway/src/sse/stream.rs`

**What happens:**
- `backend_events_to_sse()` converts `BackendEvent` → `SseEnvelope` → SSE `Event`
- Each event gets: sequential `id`, ISO 8601 `ts`, `request_id` (UUID), and `data` (the event payload)
- Events are serialized to JSON and sent as SSE `event: <type>\ndata: <json>\n\n`
- Stream terminates after `Done` or `Error` event

**SSE event types:** `message`, `status`, `error`, `done`

**Data available at output:**
```json
{
  "id": 1,
  "ts": "2024-01-15T10:30:00Z",
  "request_id": "uuid",
  "data": {
    "type": "message",
    "content": "Hello world"
  }
}
```

**What gets lost/flattened:** Nothing additional — this is a faithful serialization of `BackendEvent` to JSON. The SSE layer is lossless relative to its input.

**Injection point:** New `BackendEvent` variants would naturally map to new SSE event types. No changes needed here beyond serialization support.

---

### Hop 4b: BackendEvent → Session Store (persistence)

**File:** `dimension-gateway/src/session/handler.rs`

**What happens:**
- `SessionAwareHandler` wraps the inner handler (orchestration or remote worker)
- Before forwarding: resolves/creates session, loads history, persists user message, enriches request with session context
- During forwarding: copies events from inner mpsc to outer mpsc while **accumulating all `Message.content` into a single `String`**
- After `Done`/`Error`: persists the accumulated string as a single assistant message via `append_message()`
- Persistence is fire-and-forget (`tokio::spawn`)

**Data available at output (to session store):**
```rust
NewMessage {
    role: MessageRole::Assistant,
    content: String,          // all Message chunks concatenated
    is_complete: bool,        // true if Done, false if Error
}
```

**What gets lost/flattened:**
- ⚠️ **All streaming structure is destroyed.** The individual `Message` chunks are concatenated into one big string. There's no record of chunk boundaries, timing, or ordering.
- ⚠️ **No metadata persisted.** `Done.duration_ms`, `Done.bytes_streamed`, `Done.exit`, `Error.code` — none of these are stored.
- ⚠️ **Binary/rich content is impossible.** The store schema is a single `content: String` field.
- ⚠️ **Tool calls, artifacts, and structured data are flattened** into the text string, losing all semantic meaning for history replay.

**Injection point:**
> The `NewMessage` struct and session store schema should support:
> - `metadata: Option<serde_json::Value>` for Done metadata
> - `content_blocks: Vec<ContentBlock>` instead of a single string (matching the input format)
> - Separate storage for tool calls, artifacts, etc.

---

### Hop 4c: BackendEvent → Channel Bridge → Telegram

**File:** `dimension-gateway/src/channels/bridge.rs`, `dimension-channels/src/channels/telegram.rs`

**What happens (bridge.rs):**
1. `process_channel_message()` resolves the sender to a Dimension user
2. Builds a `MessageRequest` with session context
3. Creates an mpsc channel, spawns the handler
4. Starts Telegram typing indicator
5. **`collect_response()`** consumes all `BackendEvent`s:
   - `Message { content }` → concatenated into a single `String`
   - `Status` → ignored
   - `Error` → if no content accumulated yet, formats as `"Error (code): message"`; breaks
   - `Done` → breaks, marks `completed = true`
6. Stops typing indicator
7. Sends the accumulated response string via `channel.send()`

**What gets lost/flattened:**
- ⚠️ **This is the most aggressive flattening.** ALL streaming events are collapsed into ONE string. No metadata, no structure, no error details (if partial content exists).
- ⚠️ **Status events are completely discarded.** The user never sees "vm_spawning", "processing", etc. (typing indicator is the only UX signal).
- ⚠️ **Timing information is lost.** No `duration_ms` or `bytes_streamed` reaches the user.
- ⚠️ **Error details are lossy.** If there's partial content followed by an error, only the partial content is sent — the error is swallowed.

**What happens (telegram.rs) — sending the response:**
1. `send()` calls `strip_tool_call_tags()` to remove internal tool markers
2. `parse_attachment_markers()` extracts `[IMAGE:/path]`, `[DOCUMENT:url]`, etc. from the text
3. Text is converted from Markdown to Telegram HTML via `markdown_to_telegram_html()`
4. Messages exceeding 4096 chars are split via `split_message_for_telegram()` with continuation markers
5. Each chunk is sent via Telegram Bot API `sendMessage` with HTML parse_mode
6. If HTML parsing fails, falls back to plain text
7. Attachments are sent via `sendPhoto`, `sendDocument`, `sendVoice`, etc. (multipart upload for local files, URL for remote)

**What gets lost/flattened:**
- ⚠️ **Markdown → HTML conversion is lossy.** Not all Markdown constructs map to Telegram HTML (limited to `<b>`, `<i>`, `<code>`, `<pre>`, `<a>`, `<s>`).
- ⚠️ **Long messages are split with continuation markers** — the original structure is lost.
- ⚠️ **Attachment detection is regex-based** — relies on the agent embedding `[IMAGE:path]` markers in its text output. If the agent doesn't know about this convention, no attachments are sent.

**Injection point:**
> The bridge should be aware of structured responses:
> - Tool calls → could be rendered as inline buttons or formatted specially
> - Artifacts → sent as documents/images directly instead of requiring marker conventions
> - Metadata → could be shown as a footer ("⏱ 2.3s, 1.2k tokens")
> - Streaming mode already exists (draft updates) — structured events could drive richer progressive rendering

---

## Single-Host vs Multi-Host Path

### Single-Host Path (vsock direct)

```
Agent Process
  → child.rs (line-buffered stdout/stderr → DataChunk envelopes)
  → vsock (protobuf frames)
  → VmOrchestrationHandler.process_envelope() → BackendEvent
  → mpsc channel
  → SessionAwareHandler (accumulate + persist)
  → SSE stream / Channel bridge
```

**Characteristics:**
- Direct vsock connection between host and guest VM
- Minimal latency (no network hops)
- `VmOrchestrationHandler` owns the full VM lifecycle (spawn, connect, stream, teardown)
- `BackendEvent` is produced directly from protobuf `Envelope`

### Multi-Host Path (gRPC proxy)

```
Agent Process
  → child.rs → vsock → VmOrchestrationHandler → BackendEvent  [on WORKER node]
  → WorkerService.Execute (gRPC server-streaming)
  → backend_event_to_proto() → ProtoBackendEvent               [worker→gateway]
  → RemoteWorkerHandler.handle_message()                        [on GATEWAY node]
  → proto_to_backend_event() → BackendEvent
  → mpsc channel
  → SessionAwareHandler (accumulate + persist)
  → SSE stream / Channel bridge
```

**File:** `dimension-gateway/src/worker/remote_handler.rs`, `worker/proto_convert.rs`

**Additional hops:**
1. **Worker side:** `BackendEvent` → `ProtoBackendEvent` via `backend_event_to_proto()`
2. **gRPC transport:** protobuf frames over HTTP/2 (tonic server-streaming)
3. **Gateway side:** `ProtoBackendEvent` → `BackendEvent` via `proto_to_backend_event()`

**gRPC proto (worker_proto):**
```protobuf
message MessageChunk { string content = 1; }
message StatusEvent { string status = 1; }
message ErrorEvent { string code = 1; string message = 2; string details_json = 3; }
message DoneEvent { uint64 duration_ms = 1; uint64 bytes_streamed = 2; string exit = 3; }

message BackendEvent {
  oneof payload {
    MessageChunk message = 1;
    StatusEvent status = 2;
    ErrorEvent error = 3;
    DoneEvent done = 4;
  }
}
```

**What gets lost in the multi-host path:**
- ⚠️ **`Error.details` round-trip is lossy.** It's serialized to `details_json: String` via `serde_json::to_string()` and parsed back via `serde_json::from_str()`. If serialization fails, details become `None`.
- The conversion is otherwise **faithful** — `BackendEvent` ↔ `ProtoBackendEvent` is a 1:1 mapping.

**Dispatch logic (`DispatchingHandler`):**
- Checks bundle resource requirements (memory, vcpus)
- Calls `pick_worker()` to find a suitable worker with sufficient resources
- Falls back to local handler if no workers available
- Each request goes to exactly one worker (no fan-out)

**Key difference:** In multi-host mode, the gateway never touches vsock or protobuf `Envelope` directly. It only sees `BackendEvent` objects reconstructed from the gRPC stream. This means the gateway-side code (session handler, SSE, bridge) is **path-agnostic** — it works identically for both modes.

---

## Complete Data Flow Summary

| Stage | Data Type | Key Fields | Lost |
|-------|-----------|------------|------|
| Agent stdout | Raw text lines | Line content | Partial lines (buffered until `\n`) |
| child.rs | `Envelope<DataChunk>` | content, sequence, content_type | Nothing |
| vsock wire | Protobuf frames | All Envelope fields | Nothing |
| VmOrchestrationHandler | `BackendEvent` | content (String only) | **sequence, content_type, request_id, Done.metadata (except exit)** |
| gRPC proxy (multi-host) | `ProtoBackendEvent` | content, status, code, message, details_json, duration_ms, bytes_streamed, exit | Error details may fail round-trip |
| SessionAwareHandler | `BackendEvent` (passthrough) + accumulated String | All BackendEvent fields | Nothing (passthrough), but **persistence loses all structure** |
| Session Store | `NewMessage` | role, content (concatenated), is_complete | **All streaming structure, metadata, timing, error details** |
| SSE Stream | `SseEnvelope<SseEventData>` | Sequential JSON events | Nothing (faithful serialization) |
| Channel Bridge | Single concatenated String | Text content only | **All events collapsed to one string, status/timing/errors swallowed** |
| Telegram HTML | Formatted message | HTML subset | **Markdown fidelity, message splitting, attachment detection heuristic** |

---

## Protocol Changes Needed for Structured Responses

### 1. Protobuf Schema Extensions (dimension.proto)

```protobuf
// New payload types in Envelope.payload oneof:

message ToolCall {
  string call_id = 1;
  string tool_name = 2;
  string arguments_json = 3;
}

message ToolResult {
  string call_id = 1;
  string output_json = 2;
  bool is_error = 3;
}

message Artifact {
  string artifact_id = 1;
  string name = 2;
  string mime_type = 3;
  bytes content = 4;
  map<string, string> metadata = 5;
}

message StreamMetadata {
  map<string, string> data = 1;  // e.g., model, token_usage, etc.
}

// Updated Envelope:
message Envelope {
  string request_id = 1;
  oneof payload {
    Request request = 10;
    DataChunk data_chunk = 11;
    Error error = 12;
    Done done = 13;
    ToolCall tool_call = 14;
    ToolResult tool_result = 15;
    Artifact artifact = 16;
    StreamMetadata stream_metadata = 17;
  }
}
```

### 2. BackendEvent Extensions

```rust
enum BackendEvent {
    // Existing
    Message { content: String },
    Status { status: String },
    Error { code: String, message: String, details: Option<Value> },
    Done { duration_ms: u64, bytes_streamed: u64, exit: String, metadata: HashMap<String, String> },
    
    // New
    ToolCall { call_id: String, tool_name: String, arguments: Value },
    ToolResult { call_id: String, output: Value, is_error: bool },
    Artifact { artifact_id: String, name: String, mime_type: String, content: Vec<u8>, metadata: HashMap<String, String> },
    StreamMetadata { data: HashMap<String, String> },
}
```

### 3. Session Store Schema Changes

```sql
-- Current: single content text column
-- Proposed: structured content blocks

CREATE TABLE message_blocks (
    id UUID PRIMARY KEY,
    message_id UUID REFERENCES messages(id),
    block_type TEXT NOT NULL,  -- 'text', 'tool_call', 'tool_result', 'artifact_ref'
    content TEXT,              -- text content or JSON
    sequence INTEGER NOT NULL,
    metadata JSONB
);

CREATE TABLE artifacts (
    id UUID PRIMARY KEY,
    session_id UUID REFERENCES sessions(id),
    message_id UUID REFERENCES messages(id),
    name TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    content BYTEA NOT NULL,
    metadata JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Add metadata to messages table
ALTER TABLE messages ADD COLUMN metadata JSONB;
```

### 4. Channel Bridge Changes

The bridge needs to become structure-aware:

```rust
// Instead of collect_response() → String, use:
async fn collect_structured_response(rx: mpsc::Receiver<BackendEvent>) -> StructuredResponse {
    StructuredResponse {
        text_parts: Vec<String>,
        tool_calls: Vec<ToolCall>,
        artifacts: Vec<ArtifactRef>,
        metadata: HashMap<String, String>,
        completed: bool,
        error: Option<ErrorInfo>,
    }
}
```

### 5. Telegram Rendering of Structured Responses

```rust
// Tool calls → formatted code blocks or inline buttons
// Artifacts → sent as Telegram documents/photos
// Metadata → optional footer
// Text → existing markdown→HTML pipeline
```

### 6. gRPC Worker Proto Extensions

The `worker.proto` `BackendEvent` message needs matching new payload variants for tool calls, artifacts, and metadata to ensure multi-host mode doesn't lose structured data.

---

## Priority Order for Implementation

1. **Preserve `Done.metadata`** — smallest change, biggest immediate win. Stop dropping metadata keys in `process_envelope()`.
2. **Extend `BackendEvent` with metadata** — add `metadata: HashMap<String, String>` to `Done` variant, propagate through SSE and bridge.
3. **Content-type awareness in bridge** — use `DataChunk.content_type` to distinguish structured JSON from plain text before flattening.
4. **Session store schema** — add metadata column to messages table, persist Done metadata and error details.
5. **New protobuf payload types** — add ToolCall, Artifact, StreamMetadata to the wire protocol.
6. **End-to-end structured rendering** — bridge and Telegram channel render tool calls and artifacts natively.

---

## Appendix: File Reference

| Component | File | Role |
|-----------|------|------|
| Guest agent child process | `dimension-agent/src/child.rs` | Reads stdout/stderr, creates DataChunk envelopes |
| Wire protocol definition | `dimension-protocol/proto/dimension.proto` | Protobuf schema for all wire messages |
| Wire codec | `dimension-protocol/src/codec.rs` | Varint length-delimited framing for Envelope |
| VM orchestration | `dimension-gateway/src/orchestration/handler.rs` | Envelope → BackendEvent conversion, VM lifecycle |
| Backend event types | `dimension-gateway/src/backend/handler.rs` | BackendEvent enum, MessageHandler trait |
| SSE streaming | `dimension-gateway/src/sse/stream.rs` | BackendEvent → SSE Event serialization |
| Session persistence | `dimension-gateway/src/session/handler.rs` | Accumulate + persist responses, history enrichment |
| Channel bridge | `dimension-gateway/src/channels/bridge.rs` | BackendEvent → single string for channel delivery |
| Telegram channel | `dimension-channels/src/channels/telegram.rs` | String → Telegram Bot API (HTML, chunking, attachments) |
| gRPC remote handler | `dimension-gateway/src/worker/remote_handler.rs` | RemoteWorkerHandler, DispatchingHandler |
| gRPC proto conversion | `dimension-gateway/src/worker/proto_convert.rs` | BackendEvent ↔ ProtoBackendEvent mapping |
