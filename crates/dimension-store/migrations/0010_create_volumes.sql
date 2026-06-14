CREATE TABLE volumes (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    size_bytes  BIGINT      NOT NULL DEFAULT 10737418240,
    session_id  UUID        NULL REFERENCES sessions(id) ON DELETE SET NULL,
    worker_id   TEXT        NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_volumes_user ON volumes(user_id);
CREATE INDEX idx_volumes_session ON volumes(session_id) WHERE session_id IS NOT NULL;
