-- WP10: observability, analytics and notifications.

-- One row per agent run (agent, review, plan): the basis of the session
-- insights, the run metrics and the analytics of run times.
CREATE TABLE task_runs (
    id                 BIGSERIAL PRIMARY KEY,
    task_id            UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    user_id            UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id         UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind               TEXT NOT NULL CHECK (kind IN ('agent', 'review', 'plan')),
    -- Harness (claude|codex|gemini|opencode|mock).
    provider           TEXT NOT NULL,
    -- Price catalog id (never free text; NULL = unknown model).
    price_id           TEXT,
    started_at         TIMESTAMPTZ NOT NULL,
    ended_at           TIMESTAMPTZ NOT NULL,
    -- success | failure | task_error | rate_limit | provider_auth | provider_transient | infra
    -- | stalled | timeout | cancelled | limit | steered
    outcome            TEXT NOT NULL,
    input_tokens       BIGINT NOT NULL DEFAULT 0,
    output_tokens      BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens  BIGINT NOT NULL DEFAULT 0,
    cache_write_tokens BIGINT NOT NULL DEFAULT 0,
    tool_calls         INT NOT NULL DEFAULT 0,
    -- Sanitized tool name -> calls (at most 50 names).
    tools              JSONB NOT NULL DEFAULT '{}'
);
CREATE INDEX task_runs_task_idx ON task_runs (task_id, started_at);
CREATE INDEX task_runs_user_idx ON task_runs (user_id, started_at);
CREATE INDEX task_runs_project_idx ON task_runs (project_id, started_at);

-- History of provider pauses (provider_limits only holds the current state).
CREATE TABLE provider_pauses (
    id         BIGSERIAL PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Harness, or connector:<uuid> for a connector's own pause.
    provider   TEXT NOT NULL,
    reason     TEXT NOT NULL CHECK (reason IN ('rate_limit', 'auth', 'budget')),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Planned or actual end; NULL = until lifted (auth pauses).
    ends_at    TIMESTAMPTZ,
    CHECK (ends_at IS NULL OR ends_at >= started_at)
);
CREATE INDEX provider_pauses_user_idx ON provider_pauses (user_id, started_at);
CREATE INDEX provider_pauses_open_idx ON provider_pauses (user_id, provider, reason)
    WHERE ends_at IS NULL;

-- Analytics access paths (time windows per user / per project).
CREATE INDEX tasks_user_finished_idx ON tasks (user_id, finished_at) WHERE finished_at IS NOT NULL;
CREATE INDEX tasks_project_finished_idx ON tasks (project_id, finished_at) WHERE finished_at IS NOT NULL;
CREATE INDEX tasks_parent_idx ON tasks (parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX tasks_active_status_idx ON tasks (status)
    WHERE status IN ('pending_approval', 'queued', 'running', 'waiting_batch', 'waiting_input', 'held', 'merging');
CREATE INDEX usage_log_task_idx ON usage_log (task_id) WHERE task_id IS NOT NULL;
CREATE INDEX usage_log_user_ts_idx ON usage_log (user_id, ts);

-- Notification settings per user.
CREATE TABLE notification_settings (
    user_id            UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    -- IANA time zone of quiet hours and the daily summary.
    timezone           TEXT NOT NULL DEFAULT 'UTC',
    quiet_enabled      BOOLEAN NOT NULL DEFAULT FALSE,
    -- Minutes after local midnight; start > end spans midnight.
    quiet_start        SMALLINT NOT NULL DEFAULT 1320 CHECK (quiet_start BETWEEN 0 AND 1439),
    quiet_end          SMALLINT NOT NULL DEFAULT 420 CHECK (quiet_end BETWEEN 0 AND 1439),
    -- Critical events (approval, question, blocked) break through quiet hours.
    quiet_critical     BOOLEAN NOT NULL DEFAULT TRUE,
    digest_enabled     BOOLEAN NOT NULL DEFAULT FALSE,
    digest_minute      SMALLINT NOT NULL DEFAULT 480 CHECK (digest_minute BETWEEN 0 AND 1439),
    webhook_url        TEXT,
    webhook_secret_enc BYTEA,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Deviations from the default per event and channel.
CREATE TABLE notification_prefs (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event   TEXT NOT NULL,
    channel TEXT NOT NULL CHECK (channel IN ('inapp', 'push', 'ntfy', 'webhook')),
    enabled BOOLEAN NOT NULL,
    PRIMARY KEY (user_id, event, channel)
);

ALTER TABLE notifications
    ADD COLUMN event            TEXT,
    -- Held back from push/ntfy by quiet hours (listed in the summary).
    ADD COLUMN quiet_suppressed BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX notifications_quiet_idx ON notifications (user_id, created_at) WHERE quiet_suppressed;

-- Exactly one daily summary per user and local date (also with several
-- app instances and restarts: the primary key decides).
CREATE TABLE notification_digests (
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    local_date      DATE NOT NULL,
    period_start    TIMESTAMPTZ NOT NULL,
    period_end      TIMESTAMPTZ NOT NULL,
    -- NULL = nothing happened, nothing sent.
    notification_id UUID REFERENCES notifications(id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, local_date)
);

-- External deliveries (Web Push, ntfy, webhook) with lease and retries.
CREATE TABLE notification_outbox (
    id              BIGSERIAL PRIMARY KEY,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    notification_id UUID REFERENCES notifications(id) ON DELETE SET NULL,
    channel         TEXT NOT NULL CHECK (channel IN ('push', 'ntfy', 'webhook')),
    -- Web Push: one row per subscription (a retry never repeats other devices).
    target_id       UUID,
    event           TEXT NOT NULL,
    -- Redacted title/body/url/kind/task; never a secret.
    payload         JSONB NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts        INT NOT NULL DEFAULT 0,
    next_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until     TIMESTAMPTZ,
    last_error      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at         TIMESTAMPTZ
);
CREATE INDEX notification_outbox_due_idx ON notification_outbox (next_at) WHERE status = 'pending';
CREATE INDEX notification_outbox_user_idx ON notification_outbox (user_id, created_at);
