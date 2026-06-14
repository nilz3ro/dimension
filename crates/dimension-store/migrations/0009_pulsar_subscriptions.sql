-- Migration 0009: Pulsar channel subscription configuration
-- Stores Pulsar subscription configs for event-driven agent invocations.

CREATE TABLE pulsar_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    topic TEXT NOT NULL,
    subscription_name TEXT NOT NULL UNIQUE,
    target_bundle_id TEXT NOT NULL,
    sub_type TEXT NOT NULL CHECK (sub_type IN ('Shared', 'KeyShared')),
    max_redeliver_count INT NOT NULL DEFAULT 3,
    dead_letter_topic TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_pulsar_subscriptions_enabled ON pulsar_subscriptions (enabled);
