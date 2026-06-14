CREATE TABLE tokens (
    id          TEXT        PRIMARY KEY,
    bundle_id   TEXT        NOT NULL,
    user_id     UUID        NOT NULL,
    ciphertext  TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at  TIMESTAMPTZ NULL
);
CREATE INDEX idx_tokens_bundle ON tokens(bundle_id);
CREATE INDEX idx_tokens_expires ON tokens(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE bundle_secrets (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bundle_id   TEXT        NOT NULL,
    name        TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(user_id, bundle_id, name)
);
CREATE INDEX idx_bundle_secrets_lookup ON bundle_secrets(user_id, bundle_id);
