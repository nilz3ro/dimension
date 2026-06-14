-- Add consumer_count to pulsar_subscriptions for multi-consumer scaling.
-- Default 1 preserves existing single-consumer behavior.
ALTER TABLE pulsar_subscriptions ADD COLUMN IF NOT EXISTS consumer_count INTEGER NOT NULL DEFAULT 1;
