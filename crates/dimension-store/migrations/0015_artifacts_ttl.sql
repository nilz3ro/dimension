ALTER TABLE artifacts ADD COLUMN expires_at TIMESTAMPTZ NULL;
CREATE INDEX idx_artifacts_expires ON artifacts(expires_at) WHERE expires_at IS NOT NULL;
