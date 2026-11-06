-- WP01: structured CLI integration, failure classes and per-task limits.

-- Per-task limits: project defaults + task overrides (NULL = use the next level).
ALTER TABLE projects
    ADD COLUMN max_runtime_secs   INT              CHECK (max_runtime_secs BETWEEN 1 AND 86400),
    ADD COLUMN stall_timeout_secs INT              CHECK (stall_timeout_secs BETWEEN 0 AND 86400), -- 0 = off
    ADD COLUMN max_cost_usd       DOUBLE PRECISION CHECK (max_cost_usd > 0 AND max_cost_usd <= 10000),
    ADD COLUMN max_tokens         BIGINT           CHECK (max_tokens BETWEEN 1 AND 10000000000),
    ADD COLUMN max_turns          INT              CHECK (max_turns BETWEEN 1 AND 1000);

ALTER TABLE tasks
    ADD COLUMN max_runtime_secs  INT              CHECK (max_runtime_secs BETWEEN 1 AND 86400),
    ADD COLUMN max_cost_usd      DOUBLE PRECISION CHECK (max_cost_usd > 0 AND max_cost_usd <= 10000),
    ADD COLUMN max_tokens        BIGINT           CHECK (max_tokens BETWEEN 1 AND 10000000000),
    ADD COLUMN max_turns         INT              CHECK (max_turns BETWEEN 1 AND 1000),
    -- Why the last run did not finish normally (NULL = no failure).
    ADD COLUMN failure_class     TEXT             CHECK (failure_class IN (
        'task', 'rate_limit', 'provider_transient', 'infra', 'stalled', 'provider_auth', 'config',
        'limit_runtime', 'limit_cost', 'limit_tokens', 'limit_turns', 'cancelled')),
    -- Re-queues caused by transient provider/infra/stall failures (never fix attempts).
    ADD COLUMN transient_retries INT              NOT NULL DEFAULT 0,
    -- Backoff: the scheduler does not claim the task before this time.
    ADD COLUMN not_before        TIMESTAMPTZ;

CREATE INDEX tasks_claim_idx ON tasks (user_id, position) WHERE status = 'queued';

-- Provider paused by an auth/billing failure until credentials change or the user resumes.
ALTER TABLE provider_limits ADD COLUMN paused_reason TEXT;
