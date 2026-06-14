-- Migration 0008: A2A agent cards, agent sessions, tasks, task runs, task targets
-- All Phase 11 tables in a single migration.

-- Stores the pre-computed A2A agent card JSON per bundle.
CREATE TABLE agent_cards (
    bundle_id TEXT PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    card_json JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Persistent inter-agent sessions: caller A <-> target B per user.
CREATE TABLE agent_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    caller_bundle_id TEXT NOT NULL,
    target_bundle_id TEXT NOT NULL,
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(caller_bundle_id, target_bundle_id, user_id)
);

-- Goal-driven tasks with scheduling and iteration tracking.
CREATE TABLE tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bundle_id TEXT NOT NULL,
    goal TEXT NOT NULL,
    success_criteria TEXT,
    evaluator_bundle_id TEXT,
    trigger_type TEXT NOT NULL CHECK (trigger_type IN ('immediate', 'cron')),
    cron_expr TEXT,
    next_run_at TIMESTAMPTZ,
    max_iterations INT NOT NULL DEFAULT 25,
    timeout_hours INT NOT NULL DEFAULT 24,
    iteration_count INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','running','completed','failed','partial_success','cancelled')),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    last_output TEXT,
    last_eval_feedback TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_tasks_scheduler ON tasks(status, next_run_at) WHERE status IN ('pending', 'running');
CREATE INDEX idx_tasks_user ON tasks(user_id, status);

-- Per-iteration run records for a task.
CREATE TABLE task_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    iteration INT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    output TEXT,
    eval_feedback TEXT,
    eval_passed BOOLEAN,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_task_runs_task ON task_runs(task_id, iteration);

-- Fan-out targets for multi-target tasks (supports TASK-09).
CREATE TABLE task_targets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    bundle_id TEXT NOT NULL
);
