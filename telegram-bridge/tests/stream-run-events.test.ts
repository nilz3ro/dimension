import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

// Replace the SSE consumer with a scripted async generator. Everything else
// in dimension.js (DimensionClient, etc.) is left untouched.
const { subscribeMock } = vi.hoisted(() => ({ subscribeMock: vi.fn() }));
vi.mock('../src/dimension.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/dimension.js')>();
  return { ...actual, subscribeRunEvents: subscribeMock };
});

import { initDb, type BridgeDb } from '../src/db.js';
import { BridgeService } from '../src/bridge.js';
import type { TelegramClient } from '../src/telegram.js';
import type { DimensionClient } from '../src/dimension.js';
import type { BridgeConfig, TelegramUpdate } from '../src/types.js';

interface SseEvent {
  seq: number;
  kind: string;
  data: Record<string, unknown>;
}

/** An async generator that yields the given events in order. */
function stream(events: SseEvent[]): AsyncGenerator<SseEvent> {
  return (async function* () {
    for (const e of events) yield e;
  })();
}

function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

const TEST_CONFIG: BridgeConfig = {
  telegramBotToken: 'test-bot-token',
  dimensionApiUrl: 'https://api.test.dimension',
  dimensionApiKey: 'test-api-key',
  dimensionBundleId: 'test-bundle',
  port: 3000,
};

function makeUpdate(chatId: number, text: string, updateId = 1): TelegramUpdate {
  return {
    update_id: updateId,
    message: { message_id: updateId, chat: { id: chatId }, text },
  };
}

describe('streamRunEvents (SSE routing)', () => {
  let dir: string;
  let db: BridgeDb;
  let bridge: BridgeService;
  let sendMessageMock: ReturnType<typeof vi.fn>;
  let sendDocumentMock: ReturnType<typeof vi.fn>;
  let sendDocumentUploadMock: ReturnType<typeof vi.fn>;
  let dispatchRunMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'bridge-sse-'));
    db = initDb(join(dir, 'test.db'));
    sendMessageMock = vi.fn().mockResolvedValue(true);
    sendDocumentMock = vi.fn().mockResolvedValue(true);
    sendDocumentUploadMock = vi.fn().mockResolvedValue(true);
    dispatchRunMock = vi
      .fn()
      .mockResolvedValue({ invocation_id: 'inv-1', status: 'pending', events_url: '/runs/x/events' });

    const telegram = {
      sendMessage: sendMessageMock,
      sendDocument: sendDocumentMock,
      sendDocumentUpload: sendDocumentUploadMock,
    } as unknown as TelegramClient;
    const dimension = { dispatchRun: dispatchRunMock } as unknown as DimensionClient;
    bridge = new BridgeService(db, telegram, dimension, TEST_CONFIG);
    subscribeMock.mockReset();
  });

  afterEach(() => {
    db.close();
    rmSync(dir, { recursive: true, force: true });
    vi.restoreAllMocks();
  });

  it('forwards stdout-final as the reply and ignores message/tool/stderr', async () => {
    subscribeMock.mockReturnValue(
      stream([
        { seq: 1, kind: 'message', data: { body: { role: 'assistant', content: 'intermediate' } } },
        { seq: 2, kind: 'tool_call', data: { body: { tool_name: 'search' } } },
        { seq: 3, kind: 'stderr', data: { body: 'some log line' } },
        { seq: 4, kind: 'stdout-final', data: { body: 'The final answer' } },
        { seq: 5, kind: 'state', data: { body: { phase: 'completed' } } },
      ]),
    );

    await bridge.handleTelegramUpdate(makeUpdate(42, 'hi'));

    await vi.waitFor(() => expect(sendMessageMock).toHaveBeenCalledTimes(1));
    expect(sendMessageMock).toHaveBeenCalledWith('42', 'The final answer');
    expect(sendDocumentMock).not.toHaveBeenCalled();

    const sessionId = db.getOrCreateSession('42').session_id;
    await vi.waitFor(() => expect(db.getInFlightInvocation(sessionId)).toBeFalsy());
  });

  it('does not forward an empty stdout-final', async () => {
    subscribeMock.mockReturnValue(
      stream([
        { seq: 1, kind: 'stdout-final', data: { body: '   ' } },
        { seq: 2, kind: 'state', data: { body: { phase: 'completed' } } },
      ]),
    );

    await bridge.handleTelegramUpdate(makeUpdate(42, 'hi'));
    const sessionId = db.getOrCreateSession('42').session_id;
    await vi.waitFor(() => expect(db.getInFlightInvocation(sessionId)).toBeFalsy());
    expect(sendMessageMock).not.toHaveBeenCalled();
  });

  it('downloads a "document" event and uploads it as a file attachment', async () => {
    const pdf = new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d]); // "%PDF-"
    const fetchSpy = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(new Response(pdf, { status: 200 }));

    subscribeMock.mockReturnValue(
      stream([
        {
          seq: 1,
          kind: 'document',
          data: {
            body: {
              url: 'https://s3.local/report.pdf',
              filename: 'issues-report.pdf',
              caption: 'Issue report',
            },
          },
        },
        { seq: 2, kind: 'stdout-final', data: { body: 'Here is your report.' } },
        { seq: 3, kind: 'state', data: { body: { phase: 'completed' } } },
      ]),
    );

    await bridge.handleTelegramUpdate(makeUpdate(42, 'make a pdf'));

    await vi.waitFor(() => expect(sendDocumentUploadMock).toHaveBeenCalledTimes(1));
    expect(fetchSpy).toHaveBeenCalledWith('https://s3.local/report.pdf', expect.anything());
    const [chatArg, bytesArg, nameArg, captionArg] = sendDocumentUploadMock.mock.calls[0];
    expect(chatArg).toBe('42');
    expect(Array.from(bytesArg as Uint8Array)).toEqual([0x25, 0x50, 0x44, 0x46, 0x2d]);
    expect(nameArg).toBe('issues-report.pdf');
    expect(captionArg).toBe('Issue report');
    // The final stdout reply is still forwarded as the chat message.
    expect(sendMessageMock).toHaveBeenCalledWith('42', 'Here is your report.');
  });

  it('skips a document download that fails and still completes the run', async () => {
    const fetchSpy = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(new Response('not found', { status: 404 }));

    subscribeMock.mockReturnValue(
      stream([
        { seq: 1, kind: 'document', data: { body: { url: 'https://s3.local/gone.pdf' } } },
        { seq: 2, kind: 'state', data: { body: { phase: 'completed' } } },
      ]),
    );

    await bridge.handleTelegramUpdate(makeUpdate(42, 'make a pdf'));
    const sessionId = db.getOrCreateSession('42').session_id;
    await vi.waitFor(() => expect(db.getInFlightInvocation(sessionId)).toBeFalsy());
    expect(fetchSpy).toHaveBeenCalled();
    expect(sendDocumentUploadMock).not.toHaveBeenCalled();
  });

  it('completes the invocation and flushes queued messages on completion', async () => {
    const gate = deferred<void>();
    // First run: yield the reply, then wait on the gate before completing so we
    // can queue a second message while the invocation is still in-flight.
    subscribeMock.mockReturnValueOnce(
      (async function* () {
        yield { seq: 1, kind: 'stdout-final', data: { body: 'first reply' } };
        await gate.promise;
        yield { seq: 2, kind: 'state', data: { body: { phase: 'completed' } } };
      })(),
    );
    // Second run (the flushed batch): just complete.
    subscribeMock.mockReturnValueOnce(
      stream([{ seq: 1, kind: 'state', data: { body: { phase: 'completed' } } }]),
    );

    await bridge.handleTelegramUpdate(makeUpdate(42, 'first message'));
    await vi.waitFor(() => expect(sendMessageMock).toHaveBeenCalledWith('42', 'first reply'));

    // Queue a second message while the first invocation is still in-flight.
    await bridge.handleTelegramUpdate(makeUpdate(42, 'queued message', 2));
    const sessionId = db.getOrCreateSession('42').session_id;
    expect(db.getQueuedMessages(sessionId)).toHaveLength(1);
    expect(dispatchRunMock).toHaveBeenCalledTimes(1);

    // Release the terminal state → finalize completes + flushes the queue.
    gate.resolve();
    await vi.waitFor(() => expect(dispatchRunMock).toHaveBeenCalledTimes(2));
    expect(dispatchRunMock).toHaveBeenLastCalledWith(sessionId, 'queued message', []);
    expect(db.getQueuedMessages(sessionId)).toHaveLength(0);
  });
});
