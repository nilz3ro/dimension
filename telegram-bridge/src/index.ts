/**
 * Main entry point for the Telegram bridge service.
 *
 * Loads configuration from environment variables, starts the Fastify server,
 * and handles graceful shutdown on SIGTERM / SIGINT.
 */
import { loadConfig } from './config.js';
import { createServer } from './server.js';

async function main(): Promise<void> {
  const config = loadConfig();
  const { app, shutdown } = await createServer(config);

  // ── Graceful shutdown ────────────────────────────────────────────────────

  const signals: NodeJS.Signals[] = ['SIGTERM', 'SIGINT'];
  for (const sig of signals) {
    process.on(sig, async () => {
      app.log.info(`Received ${sig}, shutting down…`);
      try {
        await shutdown();
        app.log.info('Shutdown complete.');
      } catch (err) {
        app.log.error({ err }, 'Error during shutdown');
      }
      process.exit(0);
    });
  }

  // ── Start listening ──────────────────────────────────────────────────────

  try {
    const address = await app.listen({ host: '0.0.0.0', port: config.port });
    app.log.info(`Telegram bridge listening on ${address}`);
  } catch (err) {
    app.log.error({ err }, 'Failed to start server');
    process.exit(1);
  }
}

main();
