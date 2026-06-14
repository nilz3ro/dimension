import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { initDb, type BridgeDb } from '../src/db.js';
import { BridgeService } from '../src/bridge.js';
import type { TelegramClient } from '../src/telegram.js';
import type { DimensionClient } from '../src/dimension.js';
import type {
  TelegramUpdate,
  AgentCallbackEvent,
  BridgeConfig,
  DimensionRunResponse,
} from '../src/types.js';

// ── Helpers ─────────────────────────────────────────────────────────────────

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

const TEST_CONFIG: BridgeConfig = {
  telegramBotToken: 'test-bot-token',
  dimensionApiUrl: 'https://api.test.dimension',
  dimensionApiKey: 'test-api-key',
  dimensionBundleId: 'test-bundle',
  port: 3000,
};

// ── Test suite ──────────────────────────────────────────────────────────────

describe('BridgeService', () => {
  let dir: string;
  let db: BridgeDb;
  let telegramClient: TelegramClient;
  let dimensionClient: DimensionClient;
  let bridge: BridgeService;

  let sendMessageMock: ReturnType<typeof vi.fn>;
  let sendDocumentMock: ReturnType<typeof vi.fn>;
  let getFileMock: ReturnType<typeof vi.fn>;
  let downloadFileMock: ReturnType<typeof vi.fn>;
  let dispatchRunMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'bridge-test-'));
    db = initDb(join(dir, 'test.db'));

    sendMessageMock = vi.fn().mockResolvedValue(true);
    sendDocumentMock = vi.fn().mockResolvedValue(true);
    getFileMock = vi.fn().mockResolvedValue('photos/file_42.jpg');
    downloadFileMock = vi.fn().mockResolvedValue(new Uint8Array([1, 2, 3, 4]));
    dispatchRunMock = vi.fn().mockResolvedValue({
      invocation_id: 'inv-test-001',
      status: 'pending',
    } as DimensionRunResponse);

    telegramClient = {
      sendMessage: sendMessageMock,
      sendDocument: sendDocumentMock,
      getFile: getFileMock,
      downloadFile: downloadFileMock,
    } as unknown as TelegramClient;
    dimensionClient = { dispatchRun: dispatchRunMock } as unknown as DimensionClient;

    bridge = new BridgeService(db, telegramClient, dimensionClient, TEST_CONFIG);
  });

  afterEach(() => {
    db.close();
    rmSync(dir, { recursive: true, force: true });
  });

  // ── handleTelegramUpdate ────────────────────────────────────────────────

  it('dispatches POST /run for new message', async () => {
    const update = makeTelegramUpdate(42, 'Hello bot');
    await bridge.handleTelegramUpdate(update);

    expect(dispatchRunMock).toHaveBeenCalledOnce();
    expect(dispatchRunMock).toHaveBeenCalledWith(
      expect.any(String), // session_id (UUID)
      'Hello bot',
      [], // no images
    );
  });

  it('creates session on first message', async () => {
    const update = makeTelegramUpdate(42, 'Hello');
    await bridge.handleTelegramUpdate(update);

    // Verify session was created in DB
    const session = db.getOrCreateSession('42');
    expect(session.session_id).toBeTruthy();
    expect(session.chat_id).toBe('42');
  });

  it('queues message when invocation is in-flight', async () => {
    // First message — dispatches invocation
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'First message'));
    expect(dispatchRunMock).toHaveBeenCalledOnce();

    // Second message — should be queued because first invocation is in-flight
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Second message'));
    expect(dispatchRunMock).toHaveBeenCalledOnce(); // NOT called again

    // Verify message is in the queue
    const session = db.getOrCreateSession('42');
    const queued = db.getQueuedMessages(session.session_id);
    expect(queued).toHaveLength(1);
    expect(queued[0].text).toBe('Second message');
  });

  it('/new resets the session and does not dispatch a run', async () => {
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Hello'));
    const original = db.getOrCreateSession('42').session_id;
    dispatchRunMock.mockClear();
    sendMessageMock.mockClear();

    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, '/new'));

    expect(dispatchRunMock).not.toHaveBeenCalled();
    expect(sendMessageMock).toHaveBeenCalledOnce(); // confirmation reply
    expect(db.getOrCreateSession('42').session_id).not.toBe(original);
  });

  it('treats /reset, /New@Bot, and trailing args as the reset command', async () => {
    for (const cmd of ['/reset', '/New@nsTestingBot', '/new now please']) {
      const before = db.getOrCreateSession('42').session_id;
      await bridge.handleTelegramUpdate(makeTelegramUpdate(42, cmd));
      expect(db.getOrCreateSession('42').session_id).not.toBe(before);
    }
    expect(dispatchRunMock).not.toHaveBeenCalled();
  });

  it('downloads a photo and dispatches it as a base64 image block with the caption', async () => {
    const update: TelegramUpdate = {
      update_id: 1,
      message: {
        message_id: 1,
        chat: { id: 42 },
        caption: 'the wall is cracked',
        photo: [
          { file_id: 'small', file_unique_id: 's', width: 90, height: 60 },
          { file_id: 'big', file_unique_id: 'b', width: 800, height: 600 },
        ],
      },
    };
    await bridge.handleTelegramUpdate(update);

    // Picks the largest variant at/under the width cap (800px > 90px).
    expect(getFileMock).toHaveBeenCalledWith('big');
    expect(downloadFileMock).toHaveBeenCalledWith('photos/file_42.jpg');

    const [, text, images] = dispatchRunMock.mock.calls[0];
    expect(text).toBe('the wall is cracked');
    expect(images).toHaveLength(1);
    expect(images[0]).toMatchObject({
      mimeType: 'image/jpeg',
      data: Buffer.from([1, 2, 3, 4]).toString('base64'),
    });
  });

  it('handles a photo with no caption (empty text, image still dispatched)', async () => {
    const update: TelegramUpdate = {
      update_id: 1,
      message: {
        message_id: 1,
        chat: { id: 42 },
        photo: [{ file_id: 'big', file_unique_id: 'b', width: 800, height: 600 }],
      },
    };
    await bridge.handleTelegramUpdate(update);

    const [, text, images] = dispatchRunMock.mock.calls[0];
    expect(text).toBe('');
    expect(images).toHaveLength(1);
  });

  it('asks the user to resend a photo that arrives mid-run', async () => {
    // First text message puts an invocation in-flight.
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Hello'));
    expect(dispatchRunMock).toHaveBeenCalledTimes(1);
    dispatchRunMock.mockClear();

    await bridge.handleTelegramUpdate({
      update_id: 2,
      message: {
        message_id: 2,
        chat: { id: 42 },
        photo: [{ file_id: 'big', file_unique_id: 'b', width: 800, height: 600 }],
      },
    });

    expect(dispatchRunMock).not.toHaveBeenCalled();
    expect(sendMessageMock).toHaveBeenCalledWith(
      '42',
      expect.stringContaining('resend the photo'),
    );
  });

  it('ignores update with missing text', async () => {
    const update: TelegramUpdate = {
      update_id: 1,
      message: {
        message_id: 1,
        chat: { id: 42 },
        // no text
      },
    };
    await bridge.handleTelegramUpdate(update);

    expect(dispatchRunMock).not.toHaveBeenCalled();
  });

  it('ignores update with no message', async () => {
    const update: TelegramUpdate = { update_id: 1 };
    await bridge.handleTelegramUpdate(update);

    expect(dispatchRunMock).not.toHaveBeenCalled();
  });

  // ── handleAgentCallback ─────────────────────────────────────────────────

  it('sends response to Telegram on Message callback', async () => {
    // Set up: create session and in-flight invocation
    const session = db.getOrCreateSession('42');
    db.setInvocationInFlight(session.session_id, 'inv-001');

    const event = makeCallbackEvent(session.session_id, 'Agent reply text');
    await bridge.handleAgentCallback(event);

    expect(sendMessageMock).toHaveBeenCalledOnce();
    expect(sendMessageMock).toHaveBeenCalledWith('42', 'Agent reply text');
  });

  it('completes invocation on callback', async () => {
    const session = db.getOrCreateSession('42');
    db.setInvocationInFlight(session.session_id, 'inv-002');

    const event = makeCallbackEvent(session.session_id, 'Reply');
    await bridge.handleAgentCallback(event);

    // Invocation should no longer be in-flight
    expect(db.getInFlightInvocation(session.session_id)).toBeUndefined();
  });

  it('flushes queued messages as batch dispatch on callback', async () => {
    const session = db.getOrCreateSession('42');
    db.setInvocationInFlight(session.session_id, 'inv-003');
    db.queueMessage(session.session_id, 'queued-1');
    db.queueMessage(session.session_id, 'queued-2');

    // Reset mock to track the flush dispatch separately
    dispatchRunMock.mockResolvedValue({
      invocation_id: 'inv-flush-001',
      status: 'pending',
    });

    const event = makeCallbackEvent(session.session_id, 'Reply');
    await bridge.handleAgentCallback(event);

    // Should have dispatched with concatenated queued messages
    expect(dispatchRunMock).toHaveBeenCalledOnce();
    expect(dispatchRunMock).toHaveBeenCalledWith(
      session.session_id,
      'queued-1\nqueued-2',
      [],
    );

    // Queue should be cleared
    expect(db.getQueuedMessages(session.session_id)).toHaveLength(0);

    // New invocation should be in-flight
    const inFlight = db.getInFlightInvocation(session.session_id);
    expect(inFlight).toBeDefined();
    expect(inFlight!.invocation_id).toBe('inv-flush-001');
  });

  it('does not dispatch when no queued messages on callback', async () => {
    const session = db.getOrCreateSession('42');
    db.setInvocationInFlight(session.session_id, 'inv-004');

    const event = makeCallbackEvent(session.session_id, 'Reply');
    await bridge.handleAgentCallback(event);

    // Telegram sendMessage called, but no new dispatch
    expect(sendMessageMock).toHaveBeenCalledOnce();
    expect(dispatchRunMock).not.toHaveBeenCalled();
  });

  it('ignores callback with unknown session_id', async () => {
    const event = makeCallbackEvent('nonexistent-session', 'Reply');
    await bridge.handleAgentCallback(event);

    expect(sendMessageMock).not.toHaveBeenCalled();
    expect(dispatchRunMock).not.toHaveBeenCalled();
  });

  it('ignores non-Message event types', async () => {
    const session = db.getOrCreateSession('42');
    db.setInvocationInFlight(session.session_id, 'inv-005');

    const event = makeCallbackEvent(session.session_id, 'tool result', 'ToolCall');
    await bridge.handleAgentCallback(event);

    // Should not send to Telegram or complete invocation
    expect(sendMessageMock).not.toHaveBeenCalled();
    // Invocation should still be in-flight
    expect(db.getInFlightInvocation(session.session_id)).toBeDefined();
  });

  // ── Buffering flow ──────────────────────────────────────────────────────

  it('concatenates multiple queued messages in batch dispatch', async () => {
    const session = db.getOrCreateSession('42');
    db.setInvocationInFlight(session.session_id, 'inv-006');

    // Queue 3 messages
    db.queueMessage(session.session_id, 'msg1');
    db.queueMessage(session.session_id, 'msg2');
    db.queueMessage(session.session_id, 'msg3');

    dispatchRunMock.mockResolvedValue({
      invocation_id: 'inv-batch-001',
      status: 'pending',
    });

    const event = makeCallbackEvent(session.session_id, 'Reply');
    await bridge.handleAgentCallback(event);

    expect(dispatchRunMock).toHaveBeenCalledWith(
      session.session_id,
      'msg1\nmsg2\nmsg3',
      [],
    );
  });

  it('handles multiple rapid messages while in-flight', async () => {
    // First message dispatches
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'First'));
    expect(dispatchRunMock).toHaveBeenCalledOnce();

    // Multiple messages while in-flight — all queued
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Second'));
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Third'));
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Fourth'));

    expect(dispatchRunMock).toHaveBeenCalledOnce(); // Still only one dispatch

    const session = db.getOrCreateSession('42');
    const queued = db.getQueuedMessages(session.session_id);
    expect(queued).toHaveLength(3);
    expect(queued.map((m) => m.text)).toEqual(['Second', 'Third', 'Fourth']);
  });

  // ── Error handling ──────────────────────────────────────────────────────

  it('sends error message to Telegram when Dimension dispatch fails', async () => {
    dispatchRunMock.mockRejectedValue(new Error('Gateway unreachable'));

    const update = makeTelegramUpdate(42, 'Hello');
    await bridge.handleTelegramUpdate(update);

    // Should have sent error notification to user
    expect(sendMessageMock).toHaveBeenCalledOnce();
    expect(sendMessageMock).toHaveBeenCalledWith(
      '42',
      expect.stringContaining('unable to process'),
    );
  });

  it('ignores callback with missing session_id', async () => {
    const event = {
      session_id: '',
      event_type: 'Message',
      event_id: 'evt-bad',
      content: { role: 'assistant', content: 'test' },
      timestamp: new Date().toISOString(),
    } as AgentCallbackEvent;

    await bridge.handleAgentCallback(event);
    expect(sendMessageMock).not.toHaveBeenCalled();
  });

  it('ignores callback with missing event_type', async () => {
    const event = {
      session_id: 'some-session',
      event_type: '',
      event_id: 'evt-bad',
      content: { role: 'assistant', content: 'test' },
      timestamp: new Date().toISOString(),
    } as AgentCallbackEvent;

    await bridge.handleAgentCallback(event);
    expect(sendMessageMock).not.toHaveBeenCalled();
  });

  // ── Document event forwarding ───────────────────────────────────────────

  it('forwards Document event to Telegram via sendDocument', async () => {
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Hello'));
    const sessionId = dispatchRunMock.mock.calls[0][0];

    const docPayload = JSON.stringify({
      url: 'https://s3.example.com/issues-report.pdf?sig=abc',
      filename: 'issues-report.pdf',
      caption: 'Issue report PDF',
    });
    const event = makeCallbackEvent(sessionId, docPayload, 'Document');

    await bridge.handleAgentCallback(event);

    expect(sendDocumentMock).toHaveBeenCalledOnce();
    expect(sendDocumentMock).toHaveBeenCalledWith(
      '42',
      'https://s3.example.com/issues-report.pdf?sig=abc',
      'Issue report PDF',
    );
    // Document events alone do not complete the invocation / flush the queue —
    // the agent still sends a Message after, which is what closes the cycle.
    expect(sendMessageMock).not.toHaveBeenCalled();
  });

  it('ignores Document event with malformed JSON content', async () => {
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Hello'));
    const sessionId = dispatchRunMock.mock.calls[0][0];

    const event = makeCallbackEvent(sessionId, 'not-json', 'Document');
    await bridge.handleAgentCallback(event);

    expect(sendDocumentMock).not.toHaveBeenCalled();
  });

  it('ignores Document event missing url field', async () => {
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Hello'));
    const sessionId = dispatchRunMock.mock.calls[0][0];

    const event = makeCallbackEvent(
      sessionId,
      JSON.stringify({ caption: 'no url here' }),
      'Document',
    );
    await bridge.handleAgentCallback(event);

    expect(sendDocumentMock).not.toHaveBeenCalled();
  });

  // ── Full round-trip ─────────────────────────────────────────────────────

  it('full round-trip: send → dispatch → queue → callback → flush', async () => {
    // 1. First message dispatches
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Hello'));
    expect(dispatchRunMock).toHaveBeenCalledOnce();
    const sessionId = dispatchRunMock.mock.calls[0][0];

    // 2. While in-flight, two more messages arrive
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Follow up 1'));
    await bridge.handleTelegramUpdate(makeTelegramUpdate(42, 'Follow up 2'));
    expect(dispatchRunMock).toHaveBeenCalledOnce(); // Still one dispatch

    // 3. Agent callback arrives
    dispatchRunMock.mockResolvedValue({
      invocation_id: 'inv-round-trip',
      status: 'pending',
    });

    const event = makeCallbackEvent(sessionId, 'Agent response');
    await bridge.handleAgentCallback(event);

    // Response sent to Telegram
    expect(sendMessageMock).toHaveBeenCalledWith('42', 'Agent response');

    // Queued messages flushed as batch dispatch
    expect(dispatchRunMock).toHaveBeenCalledTimes(2); // Original + flush
    expect(dispatchRunMock).toHaveBeenLastCalledWith(
      sessionId,
      'Follow up 1\nFollow up 2',
      [],
    );
  });
});
