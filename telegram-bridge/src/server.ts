/**
 * Fastify HTTP server factory for the Telegram bridge.
 *
 * Exposes:
 *   POST /telegram/webhook — incoming Telegram updates
 *   GET  /health            — liveness probe
 *
 * Agent events arrive via the gateway's SSE stream (subscribed per run in
 * BridgeService), not via an inbound webhook.
 */
import Fastify, { type FastifyInstance } from 'fastify';
import { initDb } from './db.js';
import { TelegramClient } from './telegram.js';
import { DimensionClient } from './dimension.js';
import { BridgeService } from './bridge.js';
import { TelegramPoller } from './poller.js';
import type { BridgeConfig, TelegramUpdate } from './types.js';

export interface ServerContext {
  app: FastifyInstance;
  /** The bridge orchestrator (also the SSE event handler). Exposed for tests. */
  bridge: BridgeService;
  /** Gracefully close the database and HTTP server. */
  shutdown: () => Promise<void>;
}

/**
 * Create and configure the Fastify server.
 * Does NOT call `listen()` — the caller (index.ts) is responsible for that.
 */
export async function createServer(config: BridgeConfig): Promise<ServerContext> {
  const app = Fastify({
    logger: {
      level: 'info',
      // Redact secrets from serialised log objects
      redact: [
        'telegramBotToken',
        'dimensionApiKey',
        'req.headers.authorization',
      ],
    },
  });

  // ── Initialise dependencies ────────────────────────────────────────────────

  const dbPath = config.dbPath ?? './bridge.db';
  const db = initDb(dbPath);

  const telegramClient = new TelegramClient(config.telegramBotToken);
  const dimensionClient = new DimensionClient(
    config.dimensionApiUrl,
    config.dimensionApiKey,
    config.dimensionBundleId,
  );
  const bridge = new BridgeService(db, telegramClient, dimensionClient, config);

  // ── Inbound updates: long-polling (optional) ────────────────────────────────
  // On hosts with no public inbound path, Telegram can't reach the webhook, so
  // we pull updates instead. The webhook route below still works either way.
  let poller: TelegramPoller | undefined;
  if (config.polling) {
    poller = new TelegramPoller(telegramClient, bridge);
    await poller.start();
  }

  // ── Routes ─────────────────────────────────────────────────────────────────

  app.post<{ Body: TelegramUpdate }>('/telegram/webhook', async (request, reply) => {
    const update = request.body as TelegramUpdate;
    request.log.info({ update_id: update?.update_id }, 'telegram webhook received');
    try {
      await bridge.handleTelegramUpdate(update);
    } catch (err) {
      request.log.error({ err }, 'error handling telegram update');
    }
    return reply.code(200).send({ ok: true });
  });

  app.get('/health', async (_request, reply) => {
    return reply.code(200).send({
      status: 'ok',
      uptime: process.uptime(),
    });
  });

  // ── Global error handler ───────────────────────────────────────────────────

  app.setErrorHandler((error, request, reply) => {
    request.log.error({ err: error }, 'unhandled error');
    return reply.code(500).send({ error: 'Internal Server Error' });
  });

  // ── Shutdown helper ────────────────────────────────────────────────────────

  async function shutdown(): Promise<void> {
    if (poller) await poller.stop();
    await app.close();
    db.close();
  }

  return { app, bridge, shutdown };
}
