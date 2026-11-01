-- Projects are real git repositories (optionally with a remote) ---------------
ALTER TABLE projects
    ADD COLUMN git_remote_url TEXT,
    ADD COLUMN git_branch     TEXT NOT NULL DEFAULT 'main',
    ADD COLUMN git_token_enc  BYTEA,
    ADD COLUMN auto_push      BOOLEAN NOT NULL DEFAULT FALSE;

-- Token usage per run (rolling windows are computed from this) ----------------
CREATE TABLE usage_log (
    id                 BIGSERIAL PRIMARY KEY,
    user_id            UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider           TEXT NOT NULL,
    task_id            UUID REFERENCES tasks(id) ON DELETE SET NULL,
    ts                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    input_tokens       BIGINT NOT NULL DEFAULT 0,
    output_tokens      BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens  BIGINT NOT NULL DEFAULT 0,
    cache_write_tokens BIGINT NOT NULL DEFAULT 0,
    cost_usd           DOUBLE PRECISION NOT NULL DEFAULT 0
);
CREATE INDEX usage_log_idx ON usage_log (user_id, provider, ts);

-- User-defined token budgets per provider (NULL = unlimited) ------------------
CREATE TABLE provider_budgets (
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider        TEXT NOT NULL,
    limit_5h_tokens BIGINT,
    limit_7d_tokens BIGINT,
    limit_30d_usd   DOUBLE PRECISION,
    PRIMARY KEY (user_id, provider)
);

-- provider_limits now only holds detected subscription limits ----------------
ALTER TABLE provider_limits
    DROP COLUMN window_started_at,
    DROP COLUMN input_tokens,
    DROP COLUMN output_tokens,
    DROP COLUMN cache_read_tokens,
    DROP COLUMN cost_usd,
    DROP COLUMN runs,
    ADD COLUMN observed_limit_tokens BIGINT,
    ADD COLUMN observed_at           TIMESTAMPTZ,
    ADD COLUMN budget_notified_until TIMESTAMPTZ;
