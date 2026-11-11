-- WP07: personal API tokens, task templates, triggers (schedule, one-off,
-- API, generic webhook, GitHub) and their history.

CREATE TABLE api_tokens (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    -- `qk_` + the first characters (shown in lists, not secret).
    prefix       TEXT NOT NULL,
    -- SHA-256 (hex) of the whole token; the token itself is never stored.
    token_hash   TEXT NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    scopes       TEXT[] NOT NULL CHECK (
        cardinality(scopes) >= 1 AND scopes <@ ARRAY['projects:read', 'tasks:read', 'tasks:write', 'tasks:steer',
                                                     'triggers:read', 'triggers:write', 'triggers:fire']::TEXT[]),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Every token expires (at most 366 days, checked by the API).
    expires_at   TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ,
    revoked_at   TIMESTAMPTZ
);
CREATE INDEX api_tokens_user ON api_tokens (user_id, created_at DESC);

CREATE TABLE task_templates (
    id               UUID PRIMARY KEY,
    project_id       UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    created_by       UUID REFERENCES users(id) ON DELETE SET NULL,
    name             TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    title            TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    prompt           TEXT NOT NULL CHECK (char_length(prompt) BETWEEN 1 AND 20000),
    -- NULL = the provider/model the project or the user would choose.
    provider         TEXT,
    model            TEXT,
    -- NULL = the project's plan-mode default.
    plan_mode        BOOLEAN,
    max_runtime_secs INT,
    max_cost_usd     DOUBLE PRECISION,
    max_tokens       BIGINT,
    max_turns        INT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE triggers (
    id              UUID PRIMARY KEY,
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- Owner: the task runs with this user's access (never someone else's).
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    template_id     UUID NOT NULL REFERENCES task_templates(id) ON DELETE RESTRICT,
    kind            TEXT NOT NULL CHECK (kind IN ('schedule', 'once', 'api', 'webhook', 'github')),
    name            TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    cron            TEXT CHECK (cron IS NULL OR char_length(cron) <= 120),
    timezone        TEXT CHECK (timezone IS NULL OR char_length(timezone) <= 64),
    run_at          TIMESTAMPTZ,
    -- Next slot of a schedule / one-off trigger (NULL = nothing due).
    next_run_at     TIMESTAMPTZ,
    -- Make up a missed slot once after a downtime.
    catch_up        BOOLEAN NOT NULL DEFAULT TRUE,
    -- HMAC secret (AES-256-GCM with MASTER_KEY), webhook and GitHub only.
    secret_enc      BYTEA,
    -- queued_api::triggers::GithubFilter
    github          JSONB,
    max_per_hour    INT NOT NULL DEFAULT 10 CHECK (max_per_hour BETWEEN 1 AND 100),
    skip_if_active  BOOLEAN NOT NULL DEFAULT TRUE,
    disabled_reason TEXT CHECK (disabled_reason IS NULL OR char_length(disabled_reason) <= 500),
    last_fired_at   TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind <> 'schedule' OR (cron IS NOT NULL AND timezone IS NOT NULL)),
    CHECK (kind <> 'once' OR run_at IS NOT NULL),
    CHECK ((kind IN ('webhook', 'github')) = (secret_enc IS NOT NULL)),
    CHECK (kind <> 'github' OR github IS NOT NULL)
);
CREATE INDEX triggers_project ON triggers (project_id, created_at);
CREATE INDEX triggers_due ON triggers (next_run_at) WHERE enabled AND next_run_at IS NOT NULL;

CREATE TABLE trigger_runs (
    id            UUID PRIMARY KEY,
    trigger_id    UUID NOT NULL REFERENCES triggers(id) ON DELETE CASCADE,
    source        TEXT NOT NULL CHECK (source IN ('schedule', 'catch_up', 'manual', 'api', 'webhook', 'github')),
    outcome       TEXT NOT NULL CHECK (outcome IN ('created', 'ignored', 'skipped', 'rejected', 'failed')),
    reason        TEXT CHECK (reason IS NULL OR char_length(reason) <= 500),
    task_id       UUID REFERENCES tasks(id) ON DELETE SET NULL,
    actor_id      UUID REFERENCES users(id) ON DELETE SET NULL,
    token_id      UUID REFERENCES api_tokens(id) ON DELETE SET NULL,
    -- GitHub: `event` or `event.action`.
    event         TEXT CHECK (event IS NULL OR char_length(event) <= 100),
    -- Dedupe keys (each unique per trigger, see the partial indexes):
    -- the slot of a schedule, the delivery ID of a webhook, the SHA-256 of
    -- a GitHub body (GitHub signs no timestamp) and an event key (one task
    -- per failing commit).
    scheduled_for TIMESTAMPTZ,
    delivery_id   TEXT CHECK (delivery_id IS NULL OR char_length(delivery_id) <= 100),
    body_sha256   TEXT CHECK (body_sha256 IS NULL OR body_sha256 ~ '^[0-9a-f]{64}$'),
    dedupe_key    TEXT CHECK (dedupe_key IS NULL OR char_length(dedupe_key) <= 200),
    -- Shown in the history only (rejected deliveries keep no dedupe key, so
    -- the sender can try again).
    delivery_ref  TEXT CHECK (delivery_ref IS NULL OR char_length(delivery_ref) <= 100),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX trigger_runs_trigger ON trigger_runs (trigger_id, created_at DESC);
CREATE UNIQUE INDEX trigger_runs_slot ON trigger_runs (trigger_id, scheduled_for) WHERE scheduled_for IS NOT NULL;
CREATE UNIQUE INDEX trigger_runs_delivery ON trigger_runs (trigger_id, delivery_id) WHERE delivery_id IS NOT NULL;
CREATE UNIQUE INDEX trigger_runs_body ON trigger_runs (trigger_id, body_sha256) WHERE body_sha256 IS NOT NULL;
CREATE UNIQUE INDEX trigger_runs_key ON trigger_runs (trigger_id, dedupe_key) WHERE dedupe_key IS NOT NULL;

ALTER TABLE tasks ADD COLUMN trigger_id UUID REFERENCES triggers(id) ON DELETE SET NULL;
CREATE INDEX tasks_trigger ON tasks (trigger_id) WHERE trigger_id IS NOT NULL;
