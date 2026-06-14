-- Drop the legacy webhook delivery column. Responses are delivered via the
-- outbound event pipeline (UDS → vsock → Pulsar/ClickHouse → SSE); consumers
-- subscribe to GET /runs/{id}/events instead of registering a webhook.
ALTER TABLE sessions DROP COLUMN IF EXISTS webhook_url;
