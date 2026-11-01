-- Users & auth ---------------------------------------------------------------
CREATE TABLE users (
    id            UUID PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL DEFAULT '',
    password_hash TEXT,
    is_admin      BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE identities (
    provider   TEXT NOT NULL,
    subject    TEXT NOT NULL,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email      TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, subject)
);

CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE oauth_states (
    state         TEXT PRIMARY KEY,
    provider      TEXT NOT NULL,
    nonce         TEXT NOT NULL,
    pkce_verifier TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Per-user settings (one queue per user) -------------------------------------
CREATE TABLE user_settings (
    user_id                  UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    parallelism              INT  NOT NULL DEFAULT 1,
    default_provider         TEXT NOT NULL DEFAULT 'claude',
    onempty_enabled          BOOLEAN NOT NULL DEFAULT TRUE,
    onempty_actions          TEXT[] NOT NULL DEFAULT '{review,research,improve}',
    onempty_cooldown_minutes INT  NOT NULL DEFAULT 300,
    max_auto_tasks           INT  NOT NULL DEFAULT 2,
    max_fix_attempts         INT  NOT NULL DEFAULT 2,
    ntfy_url                 TEXT,
    last_onempty_at          TIMESTAMPTZ
);

CREATE TABLE provider_credentials (
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider   TEXT NOT NULL,
    kind       TEXT NOT NULL,
    secret_enc BYTEA NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, provider)
);

-- Subscription window tracking (5h windows) + detected rate limits
CREATE TABLE provider_limits (
    user_id           UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider          TEXT NOT NULL,
    limited_until     TIMESTAMPTZ,
    window_started_at TIMESTAMPTZ,
    input_tokens      BIGINT NOT NULL DEFAULT 0,
    output_tokens     BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens BIGINT NOT NULL DEFAULT 0,
    cost_usd          DOUBLE PRECISION NOT NULL DEFAULT 0,
    runs              INT NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, provider)
);

-- Projects (shared between tenants via membership) ---------------------------
CREATE TABLE projects (
    id              UUID PRIMARY KEY,
    owner_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    description     TEXT NOT NULL DEFAULT '',
    goal            TEXT NOT NULL DEFAULT '',
    verify_command  TEXT,
    review_enabled  BOOLEAN NOT NULL DEFAULT TRUE,
    review_provider TEXT,
    review_model    TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE project_members (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
    PRIMARY KEY (project_id, user_id)
);

-- Tasks -----------------------------------------------------------------------
CREATE SEQUENCE task_position_seq;

CREATE TABLE tasks (
    id               UUID PRIMARY KEY,
    user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id       UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    parent_id        UUID REFERENCES tasks(id) ON DELETE SET NULL,
    kind             TEXT NOT NULL DEFAULT 'task',   -- task | fix | onempty
    title            TEXT NOT NULL,
    prompt           TEXT NOT NULL,
    context          TEXT NOT NULL DEFAULT '',
    provider         TEXT NOT NULL,
    model            TEXT,
    -- pending_approval | queued | running | waiting_input | done | failed | blocked | cancelled
    status           TEXT NOT NULL,
    position         BIGINT NOT NULL DEFAULT nextval('task_position_seq'),
    attempt          INT NOT NULL DEFAULT 0,
    depth            INT NOT NULL DEFAULT 0,
    created_by_agent BOOLEAN NOT NULL DEFAULT FALSE,
    questions        JSONB,
    handoff          TEXT,
    summary          TEXT,
    error            TEXT,
    review           JSONB,
    usage            JSONB,
    snapshot_before  TEXT,
    snapshot_after   TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at       TIMESTAMPTZ,
    finished_at      TIMESTAMPTZ,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX tasks_queue_idx   ON tasks (user_id, status, position);
CREATE INDEX tasks_project_idx ON tasks (project_id, created_at DESC);

CREATE TABLE task_events (
    id      BIGSERIAL PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    ts      TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind    TEXT NOT NULL,
    content TEXT NOT NULL
);
CREATE INDEX task_events_task_idx ON task_events (task_id, id);

-- Notifications ---------------------------------------------------------------
CREATE TABLE notifications (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL DEFAULT '',
    task_id    UUID REFERENCES tasks(id) ON DELETE SET NULL,
    read_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX notifications_user_idx ON notifications (user_id, created_at DESC);

CREATE TABLE push_subscriptions (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    endpoint   TEXT NOT NULL UNIQUE,
    p256dh     TEXT NOT NULL,
    auth       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
