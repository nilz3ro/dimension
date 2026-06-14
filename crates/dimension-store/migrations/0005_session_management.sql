-- Soft-delete support for sessions
ALTER TABLE sessions ADD COLUMN deleted_at TIMESTAMPTZ NULL;

-- Active Telegram session tracking (survives server restart)
ALTER TABLE users ADD COLUMN active_telegram_session_id UUID NULL REFERENCES sessions(id);

-- Resource quota columns on users (NULL = use platform default)
ALTER TABLE users ADD COLUMN quota_max_sessions INTEGER NULL;
ALTER TABLE users ADD COLUMN quota_max_bundles INTEGER NULL;
ALTER TABLE users ADD COLUMN quota_max_concurrent_vms INTEGER NULL;

-- Index for efficient session listing by user
CREATE INDEX idx_sessions_user_updated ON sessions(user_id, updated_at DESC) WHERE deleted_at IS NULL;
