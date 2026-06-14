CREATE TYPE deployment_status AS ENUM (
    'starting',
    'health_checking',
    'healthy',
    'unhealthy',
    'stopping',
    'stopped',
    'orphaned'
);

CREATE TABLE deployments (
    id          UUID            PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID            NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bundle_id   TEXT            NOT NULL,
    name        TEXT            NOT NULL,
    status      deployment_status NOT NULL DEFAULT 'starting',
    worker_id   TEXT            NULL,
    guest_ip    TEXT            NULL,
    probe_port  INTEGER         NOT NULL DEFAULT 8080,
    pid         INTEGER         NULL,
    probe_failures INTEGER      NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ     NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ     NOT NULL DEFAULT now(),
    stopped_at  TIMESTAMPTZ     NULL
);
CREATE UNIQUE INDEX idx_deployments_user_name ON deployments(user_id, name)
    WHERE stopped_at IS NULL AND status != 'stopped';
CREATE INDEX idx_deployments_user ON deployments(user_id);
CREATE INDEX idx_deployments_worker ON deployments(worker_id) WHERE worker_id IS NOT NULL;
