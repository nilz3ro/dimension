/**
 * Telegram long-polling loop.
 *
 * Used when the bridge runs on a host with no inbound path (so Telegram can't
 * push to a webhook). The bridge makes outbound `getUpdates` calls and feeds
 * each message into the same `handleTelegramUpdate` path the webhook uses.
 *
 * Polling and webhooks are mutually exclusive on a bot, so we `deleteWebhook`
 * once at startup. Only one poller per bot token may run at a time.
 */
import type { TelegramClient } from './telegram.js';
import type { BridgeService } from './bridge.js';

const DEFAULT_TIMEOUT_SEC = 25;
const ERROR_BACKOFF_MS = 2000;

export class TelegramPoller {
  private readonly telegram: TelegramClient;
  private readonly bridge: BridgeService;
  private readonly timeoutSec: number;
  private running = false;
  private offset = 0;
  private loopDone: Promise<void> = Promise.resolve();

  constructor(
    telegram: TelegramClient,
    bridge: BridgeService,
    opts: { timeoutSec?: number } = {},
  ) {
    this.telegram = telegram;
    this.bridge = bridge;
    this.timeoutSec = opts.timeoutSec ?? DEFAULT_TIMEOUT_SEC;
  }

  /** Drop any webhook and begin polling. The loop runs until {@link stop}. */
  async start(): Promise<void> {
    if (this.running) return;
    await this.telegram.deleteWebhook();
    this.running = true;
    this.loopDone = this.loop();
    console.log('[poller] started long-polling for Telegram updates');
  }

  /** Stop polling and wait for the in-flight cycle to settle. */
  async stop(): Promise<void> {
    this.running = false;
    await this.loopDone;
  }

  private async loop(): Promise<void> {
    while (this.running) {
      try {
        const updates = await this.telegram.getUpdates(this.offset, this.timeoutSec);
        for (const update of updates) {
          // Advance the cursor first so a handler failure never reprocesses it.
          this.offset = Math.max(this.offset, update.update_id + 1);
          try {
            await this.bridge.handleTelegramUpdate(update);
          } catch (err) {
            const msg = err instanceof Error ? err.message : String(err);
            console.error(`[poller] handleTelegramUpdate failed: ${msg}`);
          }
        }
      } catch (err) {
        if (!this.running) break;
        const msg = err instanceof Error ? err.message : String(err);
        console.warn(`[poller] getUpdates error: ${msg} — backing off`);
        await sleep(ERROR_BACKOFF_MS);
      }
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
