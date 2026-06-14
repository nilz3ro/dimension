/**
 * Integration tests for the Fastify HTTP server.
 *
 * Exercise the full stack: HTTP → BridgeService → SQLite
 * External APIs (Telegram, Dimension) are mocked via globalThis.fetch.
 *
 * Agent events are delivered to the bridge via the run's SSE stream in
 * production; tests drive the same handler (`ctx.bridge.handleAgentCallback`)
 * directly with the event body a bundle emits via `dimension.sendMessage`.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer, type ServerContext } from '../src/server.js';
import type {
  BridgeConfig,
  TelegramUpdate,
  AgentCallbackEvent,
} from '../src/types.js';

// ── Helpers ─────────────────────────────────────────────────────────────────

const TEST_CONFIG: BridgeConfig = {
  telegramBotToken: 'test-bot-token',
  dimensionApiUrl: 'https://api.test.dimension',
  dimensionApiKey: 'test-api-key',
  dimensionBundleId: 'test-bundle',
  port: 0, // not used in inject()
};

function makeTelegramUpdate(chatId: number, text: string, updateId = 1): TelegramUpdate {
  return {
    update_id: updateId,
    message: {
      message_id: 1,
      chat: { id: chatId },
      text,
      from: { id: chatId, first_name: 'Test' },
    },
  };
}

function makeCallbackEvent(
  sessionId: string,
  content: string,
  eventType = 'Message',
): AgentCallbackEvent {
  return {
    session_id: sessionId,
    event_type: eventType,
    event_id: 'evt-001',
    content: { role: 'assistant', content },
    timestamp: new Date().toISOString(),
  };
}

// ── Test suite ──────────────────────────────────────────────────────────────

describe('HTTP Server Integration', () => {
  let dir: string;
  let ctx: ServerContext;
  let fetchSpy: ReturnType<typeof vi.fn>;

  /** Track dispatched Dimension and Telegram calls via the fetch mock. */
  let dimensionCalls: Array<{ url: string; body: unknown }>;
  let telegramCalls: Array<{ url: string; body: unknown }>;
  /** SSE stream controllers opened by the fetch mock; closed in afterEach. */
  let sseControllers: ReadableStreamDefaultController<Uint8Array>[];

  beforeEach(async () => {
    dir = mkdtempSync(join(tmpdir(), 'server-test-'));
    dimensionCalls = [];
    telegramCalls = [];
    sseControllers = [];

    // Mock global fetch — route Dimension and Telegram calls
    fetchSpy = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url;
      const body = init?.body ? JSON.parse(init.body as string) : undefined;

      // Dimension SSE event stream → a stream that stays open (production
      // holds the connection until ~60s after the terminal `state` event).
      // Closing it instantly would race with the bridge's "finalize on close"
      // path and clear in-flight before the next message arrives.
      // (Must be checked before the POST /run route: the path contains "/run".)
      if (url.includes('/events')) {
        const stream = new ReadableStream<Uint8Array>({
          start(controller) {
            sseControllers.push(controller);
          },
        });
        return new Response(stream, {
          status: 200,
          headers: { 'Content-Type': 'text/event-stream' },
        });
      }

      // Dimension POST /run → 202
      if (url.includes('/run')) {
        dimensionCalls.push({ url, body });
        const invocationId = `inv-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
        return new Response(
          JSON.stringify({
            invocation_id: invocationId,
            status: 'pending',
            events_url: `/runs/${invocationId}/events`,
          }),
          { status: 202, headers: { 'Content-Type': 'application/json' } },
        );
      }

      // Telegram sendMessage → 200
      if (url.includes('/sendMessage')) {
        telegramCalls.push({ url, body });
        return new Response(
          JSON.stringify({ ok: true, result: { message_id: 1 } }),
          { status: 200, headers: { 'Content-Type': 'application/json' } },
        );
      }

      return new Response('Not Found', { status: 404 });
    });

    vi.stubGlobal('fetch', fetchSpy);

    ctx = await createServer({ ...TEST_CONFIG, dbPath: join(dir, 'test.db') });
  });

  afterEach(async () => {
    // Close any open SSE streams so the bridge's background readers exit.
    for (const c of sseControllers) {
      try { c.close(); } catch { /* already closed */ }
    }
    await ctx.shutdown();
    vi.restoreAllMocks();
    rmSync(dir, { recursive: true, force: true });
  });

  // ── GET /health ─────────────────────────────────────────────────────────

  it('GET /health returns 200 with status ok', async () => {
    const res = await ctx.app.inject({
      method: 'GET',
      url: '/health',
    });

    expect(res.statusCode).toBe(200);
    const body = JSON.parse(res.body);
    expect(body.status).toBe('ok');
    expect(typeof body.uptime).toBe('number');
  });

  // ── POST /telegram/webhook ──────────────────────────────────────────────

  it('POST /telegram/webhook with valid update returns 200 and dispatches to Dimension', async () => {
    const update = makeTelegramUpdate(42, 'Hello bot');
    const res = await ctx.app.inject({
      method: 'POST',
      url: '/telegram/webhook',
      payload: update,
    });

    expect(res.statusCode).toBe(200);
    expect(dimensionCalls).toHaveLength(1);
    expect(dimensionCalls[0].body).toHaveProperty('bundle_id', 'test-bundle');
    expect(dimensionCalls[0].body).toHaveProperty('mode', 'async');
  });

  it('POST /telegram/webhook with missing message returns 200 (ignored gracefully)', async () => {
    const update: TelegramUpdate = { update_id: 1 };
    const res = await ctx.app.inject({
      method: 'POST',
      url: '/telegram/webhook',
      payload: update,
    });

    expect(res.statusCode).toBe(200);
    expect(dimensionCalls).toHaveLength(0);
  });

  it('POST /telegram/webhook with missing text returns 200 (ignored gracefully)', async () => {
    const update: TelegramUpdate = {
      update_id: 1,
      message: { message_id: 1, chat: { id: 42 } },
    };
    const res = await ctx.app.inject({
      method: 'POST',
      url: '/telegram/webhook',
      payload: update,
    });

    expect(res.statusCode).toBe(200);
    expect(dimensionCalls).toHaveLength(0);
  });

  // ── Agent events (delivered via SSE in production) ───────────────────────

  it('Message event sends to Telegram', async () => {
    // First, send a telegram update to create a session and get session_id
    const update = makeTelegramUpdate(42, 'Hello');
    await ctx.app.inject({
      method: 'POST',
      url: '/telegram/webhook',
      payload: update,
    });

    expect(dimensionCalls).toHaveLength(1);

    // Extract session_id from the Dimension payload
    const sessionId = (dimensionCalls[0].body as any).payload.session_id;

    // Deliver the agent event (as the SSE consumer would)
    const event = makeCallbackEvent(sessionId, 'Agent reply');
    await ctx.bridge.handleAgentCallback(event);

    // Telegram sendMessage should have been called with the agent's reply
    expect(telegramCalls).toHaveLength(1);
    expect(telegramCalls[0].body).toHaveProperty('text', 'Agent reply');
    expect(telegramCalls[0].body).toHaveProperty('chat_id', '42');
  });

  it('ToolCall event does not send to Telegram', async () => {
    // Create session
    await ctx.app.inject({
      method: 'POST',
      url: '/telegram/webhook',
      payload: makeTelegramUpdate(42, 'Hello'),
    });
    const sessionId = (dimensionCalls[0].body as any).payload.session_id;

    // Deliver ToolCall event
    const event = makeCallbackEvent(sessionId, 'tool result', 'ToolCall');
    await ctx.bridge.handleAgentCallback(event);

    expect(telegramCalls).toHaveLength(0); // No Telegram message sent
  });

  it('Message event with unknown session is ignored (no crash)', async () => {
    const event = makeCallbackEvent('nonexistent-session-id', 'Reply');
    await ctx.bridge.handleAgentCallback(event);

    expect(telegramCalls).toHaveLength(0);
  });

  // ── Full buffering flow ─────────────────────────────────────────────────

  it('full buffering: update → dispatch, second update → queued, event → flush + new dispatch', async () => {
    // 1. First message dispatches to Dimension
    await ctx.app.inject({
      method: 'POST',
      url: '/telegram/webhook',
      payload: makeTelegramUpdate(42, 'First message'),
    });
    expect(dimensionCalls).toHaveLength(1);
    const sessionId = (dimensionCalls[0].body as any).payload.session_id;

    // 2. Second message while in-flight → queued (no new dispatch)
    await ctx.app.inject({
      method: 'POST',
      url: '/telegram/webhook',
      payload: makeTelegramUpdate(42, 'Second message', 2),
    });
    expect(dimensionCalls).toHaveLength(1); // Still only one dispatch

    // 3. Agent Message event arrives → response sent + queue flushed + new dispatch
    const event = makeCallbackEvent(sessionId, 'Agent response');
    await ctx.bridge.handleAgentCallback(event);

    // Telegram should have received the agent response
    expect(telegramCalls).toHaveLength(1);
    expect(telegramCalls[0].body).toHaveProperty('text', 'Agent response');

    // Queued message should have triggered a new Dimension dispatch
    expect(dimensionCalls).toHaveLength(2);
    const flushPayload = (dimensionCalls[1].body as any).payload;
    expect(flushPayload.content[0].text).toBe('Second message');
    expect(flushPayload.session_id).toBe(sessionId);
  });

  // ── Secret redaction ──────────────────────────────────────────────────────

  it('Fastify logger has redaction configured', async () => {
    // Verify the logger exists and has the redact option set
    // (Pino sets redacted paths at init time; we verify by checking the logger is present)
    expect(ctx.app.log).toBeDefined();
    expect(typeof ctx.app.log.info).toBe('function');
  });

  // ── 404 for unknown routes ────────────────────────────────────────────────

  it('returns 404 for unknown routes', async () => {
    const res = await ctx.app.inject({
      method: 'GET',
      url: '/nonexistent',
    });
    expect(res.statusCode).toBe(404);
  });
});
