import type { BridgeConfig } from './types.js';

/**
 * Read and validate bridge configuration from environment variables.
 * Throws on any missing required variable.
 */
export function loadConfig(): BridgeConfig {
  const required: Record<string, string | undefined> = {
    TELEGRAM_BOT_TOKEN: process.env.TELEGRAM_BOT_TOKEN,
    DIMENSION_API_URL: process.env.DIMENSION_API_URL,
    DIMENSION_API_KEY: process.env.DIMENSION_API_KEY,
    DIMENSION_BUNDLE_ID: process.env.DIMENSION_BUNDLE_ID,
  };

  const missing = Object.entries(required)
    .filter(([, v]) => !v)
    .map(([k]) => k);

  if (missing.length > 0) {
    throw new Error(
      `Missing required environment variables: ${missing.join(', ')}`,
    );
  }

  const port = parseInt(process.env.BRIDGE_PORT ?? '3000', 10);
  if (Number.isNaN(port) || port < 0 || port > 65535) {
    throw new Error(`Invalid BRIDGE_PORT: ${process.env.BRIDGE_PORT}`);
  }

  // Long-polling mode: the bridge pulls updates from Telegram instead of
  // receiving webhook pushes. Use this on hosts with no public inbound path.
  const polling = (process.env.BRIDGE_POLLING ?? '').toLowerCase() === 'true';

  return {
    telegramBotToken: required.TELEGRAM_BOT_TOKEN!,
    dimensionApiUrl: required.DIMENSION_API_URL!,
    dimensionApiKey: required.DIMENSION_API_KEY!,
    dimensionBundleId: required.DIMENSION_BUNDLE_ID!,
    port,
    polling,
  };
}
