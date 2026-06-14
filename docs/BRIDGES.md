# Building a Dimension Bridge

A bridge connects an external platform to the Dimension gateway. It translates platform-specific events (messages, reactions, commands) into `POST /run` dispatches, consumes the run's Server-Sent Events stream, and delivers the agent's output back to users on the platform.

There are exactly two ways to consume Dimension:

1. **Direct** — your application calls `POST /run` and subscribes to `GET /runs/{id}/events` itself.
2. **Via a bridge** — a small always-on service does that on behalf of a platform that can't (Telegram, Slack, Discord, cron jobs, other systems).

This guide covers the bridge pattern, using the TypeScript Telegram bridge (`telegram-bridge/`) as the reference implementation.

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [How a Bridge Works](#how-a-bridge-works)
3. [Core Components](#core-components)
4. [The Dimension Client](#the-dimension-client)
5. [Consuming the SSE Stream](#consuming-the-sse-stream)
6. [The Mapping Store](#the-mapping-store)
7. [Buffering In-Flight Conversations](#buffering-in-flight-conversations)
8. [Failure Handling](#failure-handling)
9. [Deployment](#deployment)
10. [Testing Strategy](#testing-strategy)
11. [Platform-Specific Considerations](#platform-specific-considerations)
12. [Reference: Telegram Bridge Source Map](#reference-telegram-bridge-source-map)

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────────────┐
│                        MESSAGING PLATFORM                            │
│                  (Telegram, Slack, Discord, …)                       │
└──────────────┬───────────────────────────────────▲──────────────────┘
               │ inbound webhook                    │ send message API
               ▼                                    │
┌──────────────────────────────────────────────────┴──────────────────┐
│                              BRIDGE                                  │
│  ┌──────────────┐   ┌─────────────┐   ┌──────────────────────────┐  │
│  │  HTTP server  │   │  Mapping DB │   │  Dimension client        │  │
│  │  (platform    │   │  (SQLite)   │   │  POST /run               │  │
│  │   webhook)    │   │             │   │  GET /runs/{id}/events   │  │
│  └──────────────┘   └─────────────┘   │  (SSE consumer)          │  │
│                                        └──────────────────────────┘  │
└──────────────────────────────────────────────┬───────────▲──────────┘
                                               │ POST /run │ SSE
                                               ▼           │
┌──────────────────────────────────────────────────────────┴──────────┐
│                        DIMENSION GATEWAY                             │
│   POST /run ──▶ Spawn VM ──▶ Run agent ──▶ run events ──▶ SSE        │
└──────────────────────────────────────────────────────────────────────┘
```

Note the direction of every arrow between the bridge and Dimension: **the bridge initiates both connections.** Dimension never calls back into the bridge, so the bridge needs no public URL for Dimension — only the platform (e.g. Telegram) needs to reach it.

---

## How a Bridge Works

One full round trip:

```
User              Platform           Bridge                Dimension
 │  "hello"          │                  │                      │
 │ ─────────────────▶│  POST /webhook   │                      │
 │                   │ ────────────────▶│                      │
 │                   │                  │ 1. map chat → session│
 │                   │                  │ 2. POST /run (async) │
 │                   │                  │ ────────────────────▶│
 │                   │                  │  202 {invocation_id, │
 │                   │                  │       events_url}    │
 │                   │                  │ ◀────────────────────│
 │                   │                  │ 3. GET events (SSE)  │
 │                   │                  │ ────────────────────▶│
 │                   │                  │   bundle: Message    │
 │                   │                  │ ◀════════════════════│
 │                   │  sendMessage     │ 4. forward to user   │
 │                   │ ◀────────────────│                      │
 │ ◀─────────────────│                  │   state: completed   │
 │                   │                  │ ◀════════════════════│
 │                   │                  │ 5. mark invocation   │
 │                   │                  │    done, flush queue │
```

1. The platform delivers a user message to the bridge's webhook endpoint.
2. The bridge resolves (or creates) the session for that chat, and dispatches `POST /run` with `mode: "async"`, passing the message and any history it tracks in `payload`.
3. The bridge immediately subscribes to the returned `events_url` (SSE).
4. `bundle` events carrying agent output (emitted by the agent via `dimension.sendMessage(...)`) are forwarded to the platform.
5. The terminal `state` event (`completed` / `process_exited`) marks the invocation done; any messages the user sent while the run was in flight are flushed as a new dispatch.

---

## Core Components

| Component | Responsibility | Telegram bridge file |
|-----------|----------------|----------------------|
| HTTP server | Receive platform webhooks, health checks | `src/server.ts` |
| Bridge service | Orchestrate dispatch, queueing, event handling | `src/bridge.ts` |
| Dimension client | `POST /run` with retry, SSE subscription | `src/dimension.ts` |
| Platform client | Send messages/documents back to the platform | `src/telegram.ts` |
| Mapping store | chat ↔ session, in-flight invocations, queue | `src/db.ts` (better-sqlite3) |
| Config | Env-var driven configuration | `src/config.ts` |

Required configuration (Telegram bridge):

```
TELEGRAM_BOT_TOKEN    # platform credential
DIMENSION_API_URL     # gateway base URL
DIMENSION_API_KEY     # bearer token for the gateway
DIMENSION_BUNDLE_ID   # which bundle handles this platform's messages
BRIDGE_PORT           # local HTTP port (default 3000)
```

---

## The Dimension Client

Dispatch runs with `mode: "async"` and retry on 5xx/network errors:

```typescript
const resp = await fetch(`${apiUrl}/run`, {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
    Authorization: `Bearer ${apiKey}`,
  },
  body: JSON.stringify({
    bundle_id: bundleId,
    mode: "async",
    payload: {
      role: "user",
      content: [{ type: "text", text: messageText }],
      session_id: sessionId,
      bundle_id: bundleId,
      history: [],
      truncation: { total_messages: 0, included_messages: 0, truncated: false },
    },
  }),
  signal: AbortSignal.timeout(30_000),
});
// → 202 { invocation_id, status, events_url }
```

See `telegram-bridge/src/dimension.ts` for the full implementation with exponential backoff.

---

## Consuming the SSE Stream

Subscribe to `events_url` right after dispatch. Each SSE frame has an `event:` name (`state` or `bundle`), an `id:` (sequence number), and a JSON `data:` body.

```typescript
for await (const ev of subscribeRunEvents(apiUrl, apiKey, eventsUrl)) {
  if (ev.kind === "bundle") {
    // data.body is what the agent passed to dimension.send({ body }).
    // dimension.sendMessage(...) produces this shape:
    // { session_id, event_type, event_id, content: { role, content }, timestamp }
    const payload = parseBody(ev.data.body);
    if (payload?.event_type === "Message") {
      await platform.sendMessage(chatId, payload.content.content);
    } else if (payload?.event_type === "Document") {
      // content.content is JSON: { url, filename?, caption? }
      await platform.sendDocument(chatId, ...);
    }
    // ToolCall / ToolResult events are informational — log, don't forward.
  } else if (ev.kind === "state") {
    const phase = ev.data.body?.phase;
    if (phase === "completed" || phase === "process_exited") {
      completeInvocation(invocationId);
      flushQueuedMessages(sessionId);
    }
  }
}
```

Two properties make this robust:

- **Replay** — the gateway replays history from ClickHouse on connect, so subscribing after the agent already emitted events loses nothing.
- **Resume** — pass `Last-Event-ID: <seq>` on reconnect to skip already-seen events.

The stream is closed by the server shortly after the run completes. The reference consumer also caps stream lifetime with `AbortSignal.timeout(...)` so a hung connection can't block an invocation forever (`telegram-bridge/src/dimension.ts`).

---

## The Mapping Store

The bridge owns three small tables (SQLite via better-sqlite3, WAL mode):

| Table | Purpose |
|-------|---------|
| `sessions` | `chat_id ↔ session_id` — one Dimension session per platform chat |
| `invocations` | in-flight / completed / failed runs per session |
| `queued_messages` | messages received while a run is in flight |

Dimension itself is stateless about your platform: the bridge decides what a "conversation" is and what history to send in each `payload`.

---

## Buffering In-Flight Conversations

Only one run per session should be active at a time. When a user sends a message while a run is in flight:

1. Queue it (`queued_messages`).
2. When the terminal `state` event arrives, concatenate the queued messages and dispatch them as a single new run.

This is implemented in `BridgeService.handleTelegramUpdate` / `streamRunEvents` (`telegram-bridge/src/bridge.ts`).

---

## Failure Handling

- **Dispatch failure** (after retries): tell the user, don't create an in-flight record.
- **SSE stream failure**: mark the invocation failed (`failInvocation`) so the session doesn't stay stuck, then flush any queued messages as a fresh dispatch.
- **Platform delivery failure**: retry with backoff (`postWithRetry` in `src/telegram.ts`); Telegram 429s include a `retry_after` to honor.

---

## Deployment

```
Internet ──▶ reverse proxy / tunnel ──▶ bridge :3000 ──▶ gateway :3000
   (platform webhooks only)
```

1. **Public HTTPS URL for the platform only.** Telegram needs to reach `POST /telegram/webhook`; use a reverse proxy (nginx, Caddy) or tunnel (Cloudflare Tunnel) for that. Dimension never calls the bridge, so the bridge↔gateway link can stay on a private network.
2. **Register the platform webhook** (Telegram: `setWebhook` with your public URL).
3. **Run as a service.** Build with `npm ci && npm run build`, then run `node dist/index.js` under a process manager (systemd, pm2, a container, …) with the env vars from `src/config.ts` set.

---

## Testing Strategy

The Telegram bridge's vitest suite (43 tests, `telegram-bridge/tests/`) shows the pattern:

- **`db.test.ts`** — mapping-store unit tests against a temp SQLite file.
- **`bridge.test.ts`** — orchestration tests with mocked platform/Dimension clients.
- **`server.test.ts`** — full-stack tests: real Fastify + real SQLite, `globalThis.fetch` mocked to play both the gateway (`/run` → 202 with `events_url`, `/events` → SSE) and the platform API. Agent events are injected through the same handler the SSE consumer uses.

---

## Platform-Specific Considerations

| Platform | Send API | Chat identifier | Notes |
|----------|----------|-----------------|-------|
| Telegram | `POST /bot<token>/sendMessage` | `chat_id` (number) | 4096-char message limit; split long replies |
| Slack | `POST /chat.postMessage` | `channel` | Use Events API for inbound |
| Discord | `POST /channels/{id}/messages` | `channel_id` (snowflake) | 2000-char limit |
| WhatsApp | `POST /v1/messages` | `to` (phone number) | Template restrictions outside 24h window |

Always verify inbound webhook authenticity (signature header or secret token) — anyone who can reach your webhook endpoint can otherwise impersonate users.

---

## Reference: Telegram Bridge Source Map

```
telegram-bridge/
├── src/
│   ├── index.ts      # entry point: config → server → listen, graceful shutdown
│   ├── server.ts     # Fastify: POST /telegram/webhook, GET /health
│   ├── bridge.ts     # BridgeService: dispatch, SSE event handling, queueing
│   ├── dimension.ts  # DimensionClient (POST /run) + subscribeRunEvents (SSE)
│   ├── telegram.ts   # Telegram API client (sendMessage, sendDocument, retry)
│   ├── db.ts         # better-sqlite3 store: sessions, invocations, queue
│   ├── config.ts     # env-var config loading/validation
│   └── types.ts      # shared types (payloads, events, config)
└── tests/            # vitest: db, bridge, server suites
```
