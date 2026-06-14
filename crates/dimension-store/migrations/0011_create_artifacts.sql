CREATE TABLE artifacts (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id   UUID        NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    user_id      UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    object_key   TEXT        NOT NULL,
    size_bytes   BIGINT      NOT NULL,
    content_type TEXT        NULL,
    checksum     TEXT        NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(session_id, object_key)
);
CREATE INDEX idx_artifacts_session ON artifacts(session_id, created_at DESC);
CREATE INDEX idx_artifacts_user ON artifacts(user_id, created_at DESC);
