CREATE TABLE bundle_storage (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bundle_id   TEXT        NOT NULL,
    bytes_used  BIGINT      NOT NULL DEFAULT 0,
    quota_bytes BIGINT      NOT NULL DEFAULT 104857600,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(user_id, bundle_id)
);
CREATE INDEX idx_bundle_storage_lookup ON bundle_storage(user_id, bundle_id);
