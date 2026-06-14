import { describe, it, expect, vi } from 'vitest';
import { TelegramPoller } from '../src/poller.js';
import type { TelegramClient } from '../src/telegram.js';
import type { BridgeService } from '../src/bridge.js';
import type { TelegramUpdate } from '../src/types.js';

function update(updateId: number, text: string): TelegramUpdate {
  return { update_id: updateId, message: { message_id: updateId, chat: { id: 1 }, text } };
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

describe('TelegramPoller', () => {
  it('deletes the webhook, feeds updates to the bridge, and advances the offset', async () => {
    const handled: TelegramUpdate[] = [];
    const offsets: number[] = [];
    const deleteWebhook = vi.fn().mockResolvedValue(true);

    // First poll yields two updates; later polls simulate a blocking long-poll
    // that returns nothing (so the loop doesn't spin).
    const getUpdates = vi.fn(async (offset: number) => {
      offsets.push(offset);
      if (getUpdates.mock.calls.length === 1) return [update(10, 'a'), update(11, 'b')];
      await sleep(20);
      return [];
    });

    const telegram = { deleteWebhook, getUpdates } as unknown as TelegramClient;
    const bridge = {
      handleTelegramUpdate: vi.fn(async (u: TelegramUpdate) => {
        handled.push(u);
      }),
    } as unknown as BridgeService;

    const poller = new TelegramPoller(telegram, bridge, { timeoutSec: 0 });
    await poller.start();

    await vi.waitFor(() => expect(handled).toHaveLength(2));
    await poller.stop();

    expect(deleteWebhook).toHaveBeenCalledOnce();
    expect(handled.map((u) => u.update_id)).toEqual([10, 11]);
    // First poll starts at offset 0; after consuming up to update_id 11, the
    // next poll uses offset 12.
    expect(offsets[0]).toBe(0);
    expect(offsets).toContain(12);
  });

  it('backs off and continues after a getUpdates error', async () => {
    const deleteWebhook = vi.fn().mockResolvedValue(true);
    const getUpdates = vi.fn(async () => {
      const n = getUpdates.mock.calls.length;
      if (n === 1) throw new Error('network blip');
      if (n === 2) return [update(5, 'after error')];
      await sleep(20);
      return [];
    });
    const handle = vi.fn().mockResolvedValue(undefined);

    const telegram = { deleteWebhook, getUpdates } as unknown as TelegramClient;
    const bridge = { handleTelegramUpdate: handle } as unknown as BridgeService;

    const poller = new TelegramPoller(telegram, bridge, { timeoutSec: 0 });
    await poller.start();

    await vi.waitFor(() => expect(handle).toHaveBeenCalledTimes(1), { timeout: 5000 });
    await poller.stop();
    expect(getUpdates.mock.calls.length).toBeGreaterThanOrEqual(2);
  });
});
