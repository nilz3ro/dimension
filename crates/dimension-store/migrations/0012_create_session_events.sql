CREATE TABLE session_events (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id  UUID        NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    event_type  TEXT        NOT NULL CHECK (event_type IN ('message', 'tool_call', 'tool_result')),
    role        TEXT        NULL,
    content     TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_session_events_session ON session_events(session_id, created_at, id);
