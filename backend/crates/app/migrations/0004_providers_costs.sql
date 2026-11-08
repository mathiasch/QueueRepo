-- WP13: provider connectors, price catalog overrides, cost accounting,
-- per-step routing, batch jobs and the eval set.

-- Connectors: additional, explicitly configured accesses (API keys, Bedrock,
-- Vertex, gateways, OpenAI-compatible endpoints …). The legacy
-- provider_credentials keep working unchanged.
CREATE TABLE connectors (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- NULL = usable for all tasks of the user; otherwise only in this project
    -- (by every editor of the project).
    project_id UUID REFERENCES projects(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    label      TEXT NOT NULL CHECK (length(label) BETWEEN 1 AND 100),
    config     JSONB NOT NULL DEFAULT '{}',
    -- AES-256-GCM (crypto.rs); write-only through the API.
    secret_enc BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX connectors_user_idx ON connectors (user_id);
CREATE INDEX connectors_project_idx ON connectors (project_id) WHERE project_id IS NOT NULL;

-- Per-step routing and data residency per project.
ALTER TABLE projects
    ADD COLUMN routing        JSONB,
    ADD COLUMN data_residency TEXT NOT NULL DEFAULT 'any' CHECK (data_residency IN ('any', 'eu', 'ch')),
    ADD COLUMN is_eval        BOOLEAN NOT NULL DEFAULT FALSE;

-- Tasks: connector of the agent loop, escalation step, plan of the plan step,
-- money cost per step. New status value 'waiting_batch' (no CHECK on status).
ALTER TABLE tasks
    ADD COLUMN connector_id     UUID REFERENCES connectors(id) ON DELETE SET NULL,
    ADD COLUMN escalation_level INT NOT NULL DEFAULT 0 CHECK (escalation_level BETWEEN 0 AND 10),
    ADD COLUMN plan             TEXT,
    ADD COLUMN cost             JSONB;

-- Cost accounting from token counters × price catalog.
ALTER TABLE usage_log
    ADD COLUMN project_id   UUID REFERENCES projects(id) ON DELETE SET NULL,
    ADD COLUMN connector_id UUID REFERENCES connectors(id) ON DELETE SET NULL,
    ADD COLUMN model        TEXT,
    ADD COLUMN step         TEXT NOT NULL DEFAULT 'execute' CHECK (step IN ('execute', 'plan', 'review', 'analyze')),
    ADD COLUMN cost_source  TEXT NOT NULL DEFAULT 'cli' CHECK (cost_source IN ('catalog', 'cli', 'unknown')),
    ADD COLUMN currency     TEXT NOT NULL DEFAULT 'USD' CHECK (currency IN ('USD', 'CHF')),
    ADD COLUMN cost_amount  DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN price_id     TEXT;
CREATE INDEX usage_log_project_idx ON usage_log (project_id, ts);

-- Single-shot steps (plan, review, OnEmpty analysis) as direct API calls,
-- synchronous or through a provider batch API. Persistent: survives restarts.
CREATE TABLE batch_jobs (
    id           UUID PRIMARY KEY,
    task_id      UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    step         TEXT NOT NULL CHECK (step IN ('plan', 'review', 'analyze')),
    mode         TEXT NOT NULL CHECK (mode IN ('sync', 'batch')),
    api          TEXT NOT NULL CHECK (api IN ('anthropic', 'openai', 'mistral', 'openai_chat')),
    connector_id UUID REFERENCES connectors(id) ON DELETE SET NULL,
    model        TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('pending', 'submitted', 'succeeded', 'failed', 'expired', 'cancelled')),
    external_id  TEXT,
    -- Request body without any secret, plus data to continue the task.
    request      JSONB NOT NULL,
    payload      JSONB NOT NULL DEFAULT '{}',
    result       TEXT,
    usage        JSONB,
    error        TEXT,
    attempts     INT NOT NULL DEFAULT 0,
    next_poll_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at  TIMESTAMPTZ
);
CREATE INDEX batch_jobs_poll_idx ON batch_jobs (next_poll_at) WHERE status IN ('pending', 'submitted');
CREATE INDEX batch_jobs_task_idx ON batch_jobs (task_id);

-- Admin overrides of the built-in price catalog; every change is a new version.
CREATE TABLE price_overrides (
    version    SERIAL PRIMARY KEY,
    entries    JSONB NOT NULL,
    note       TEXT NOT NULL DEFAULT '',
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Eval set runs (basis for routing calibration, WP15).
CREATE TABLE eval_runs (
    id          UUID PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    target      JSONB NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('running', 'finished')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

CREATE TABLE eval_results (
    run_id     UUID NOT NULL REFERENCES eval_runs(id) ON DELETE CASCADE,
    case_id    TEXT NOT NULL,
    task_id    UUID REFERENCES tasks(id) ON DELETE SET NULL,
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    PRIMARY KEY (run_id, case_id)
);
