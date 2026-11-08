-- WP03: gate and supply-chain protection.

-- Protection settings per project (owner only).
ALTER TABLE projects
    -- Agents may load hooks/MCP servers/settings from the repository.
    ADD COLUMN trust_repo_config  BOOLEAN NOT NULL DEFAULT FALSE,
    -- Protected path globs; NULL = built-in default list.
    ADD COLUMN protected_paths    TEXT[] CHECK (protected_paths IS NULL OR cardinality(protected_paths) <= 100),
    -- Detect skipped/deleted tests, removed assertions, verifier config changes.
    ADD COLUMN test_guard         BOOLEAN NOT NULL DEFAULT TRUE,
    -- Approval before a setup with secrets runs changed setup files.
    ADD COLUMN setup_guard        BOOLEAN NOT NULL DEFAULT TRUE,
    -- HMAC fingerprints of approved secret findings (never the values).
    ADD COLUMN secret_allowlist   TEXT[] NOT NULL DEFAULT '{}';

ALTER TABLE tasks
    -- Current hold (stage, masked findings, resume point); NULL = none.
    ADD COLUMN gate               JSONB,
    -- Internal state needed to continue after an approval (summary, follow-ups …).
    ADD COLUMN gate_pending       JSONB,
    -- Test task created by a human: protected paths/tests need no approval.
    ADD COLUMN allow_test_changes BOOLEAN NOT NULL DEFAULT FALSE;

-- New failure class 'gate' (a human rejected a gate). New status 'held'
-- (no CHECK on tasks.status).
ALTER TABLE tasks DROP CONSTRAINT tasks_failure_class_check;
ALTER TABLE tasks ADD CONSTRAINT tasks_failure_class_check CHECK (failure_class IN (
    'task', 'rate_limit', 'provider_transient', 'infra', 'stalled', 'provider_auth', 'config',
    'limit_runtime', 'limit_cost', 'limit_tokens', 'limit_turns', 'cancelled', 'gate'));
