-- WP08: intake integrations (GitHub issues, Linear agent sessions, Slack,
-- Jira): connections, external → internal identities, task links, the
-- outbox of status updates and the event history.

CREATE TABLE integrations (
    id              UUID PRIMARY KEY,
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- Who connected it (the project owner at that time). With
    -- `config.allow_unmapped` unmapped people act as this user.
    owner_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('github', 'linear', 'slack', 'jira')),
    name            TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    -- The external tenant: GitHub `owner/repo` (lower case), Slack team ID,
    -- Linear organization ID, Jira site URL. One tenant = one project.
    tenant          TEXT NOT NULL CHECK (char_length(tenant) BETWEEN 1 AND 200),
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    -- Webhook signing secret (AES-256-GCM with MASTER_KEY).
    secret_enc      BYTEA NOT NULL,
    -- API token for status updates (AES-256-GCM); NULL = no updates.
    token_enc       BYTEA,
    -- Jira Cloud: e-mail address of the API token's account.
    account         TEXT CHECK (account IS NULL OR char_length(account) <= 200),
    -- queued_api::intake::IntakeConfig
    config          JSONB NOT NULL DEFAULT '{}'::JSONB,
    max_per_hour    INT NOT NULL DEFAULT 10 CHECK (max_per_hour BETWEEN 1 AND 100),
    disabled_reason TEXT CHECK (disabled_reason IS NULL OR char_length(disabled_reason) <= 500),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (kind, tenant)
);
CREATE INDEX integrations_project ON integrations (project_id, created_at);

-- External person → Queue user. Active only when the project owner
-- approved it (`owner_ok`: who may act in the project) AND the user
-- confirmed it (`user_ok`: whose access is used).
CREATE TABLE intake_identities (
    id             UUID PRIMARY KEY,
    integration_id UUID NOT NULL REFERENCES integrations(id) ON DELETE CASCADE,
    external_id    TEXT NOT NULL CHECK (char_length(external_id) BETWEEN 1 AND 200),
    external_name  TEXT NOT NULL DEFAULT '' CHECK (char_length(external_name) <= 200),
    user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    owner_ok       BOOLEAN NOT NULL DEFAULT FALSE,
    user_ok        BOOLEAN NOT NULL DEFAULT FALSE,
    created_by     UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (integration_id, external_id)
);
CREATE INDEX intake_identities_user ON intake_identities (user_id);

-- Task ↔ external object (issue, agent session, Slack thread).
CREATE TABLE intake_links (
    id             UUID PRIMARY KEY,
    integration_id UUID NOT NULL REFERENCES integrations(id) ON DELETE CASCADE,
    task_id        UUID NOT NULL UNIQUE REFERENCES tasks(id) ON DELETE CASCADE,
    -- Key of the external object (e.g. `issue:12`, `PROJ-7`, a session ID,
    -- `C123:1700000000.000100`).
    external_ref   TEXT NOT NULL CHECK (char_length(external_ref) BETWEEN 1 AND 300),
    external_url   TEXT CHECK (external_url IS NULL OR char_length(external_url) <= 500),
    -- Untrusted (shown escaped in the UI, never in a task title or prompt).
    external_title TEXT CHECK (external_title IS NULL OR char_length(external_title) <= 300),
    -- Where updates go (kind specific, no secrets).
    reply_to       JSONB NOT NULL DEFAULT '{}'::JSONB,
    -- Key of the last phase put into the outbox.
    reported       TEXT CHECK (reported IS NULL OR char_length(reported) <= 200),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX intake_links_ref ON intake_links (integration_id, external_ref, created_at DESC);

-- Outgoing updates (comment, thread reply, agent activity). One row per
-- integration and key: a phase is never sent twice.
CREATE TABLE intake_outbox (
    id              UUID PRIMARY KEY,
    integration_id  UUID NOT NULL REFERENCES integrations(id) ON DELETE CASCADE,
    link_id         UUID REFERENCES intake_links(id) ON DELETE CASCADE,
    key             TEXT NOT NULL CHECK (char_length(key) BETWEEN 1 AND 200),
    kind            TEXT NOT NULL CHECK (kind IN ('accepted', 'question', 'done', 'failed', 'cancelled', 'refused')),
    body            TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 6000),
    reply_to        JSONB NOT NULL DEFAULT '{}'::JSONB,
    -- ID of the created comment/message/activity (Linear: chosen up front).
    remote_id       TEXT CHECK (remote_id IS NULL OR char_length(remote_id) <= 200),
    state           TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'sent', 'failed')),
    attempts        INT NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until     TIMESTAMPTZ,
    last_error      TEXT CHECK (last_error IS NULL OR char_length(last_error) <= 500),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at         TIMESTAMPTZ,
    UNIQUE (integration_id, key),
    CHECK ((state = 'sent') = (sent_at IS NOT NULL))
);
CREATE INDEX intake_outbox_due ON intake_outbox (next_attempt_at) WHERE state = 'pending';
CREATE INDEX intake_outbox_link ON intake_outbox (link_id, created_at);

-- What happened to each delivery (dedupe keys unique per integration).
CREATE TABLE intake_events (
    id             UUID PRIMARY KEY,
    integration_id UUID NOT NULL REFERENCES integrations(id) ON DELETE CASCADE,
    outcome        TEXT NOT NULL CHECK (outcome IN ('created', 'answered', 'steered', 'continued', 'ignored', 'rejected', 'failed')),
    reason         TEXT CHECK (reason IS NULL OR char_length(reason) <= 500),
    event          TEXT CHECK (event IS NULL OR char_length(event) <= 100),
    external_user  TEXT CHECK (external_user IS NULL OR char_length(external_user) <= 200),
    task_id        UUID REFERENCES tasks(id) ON DELETE SET NULL,
    delivery_id    TEXT CHECK (delivery_id IS NULL OR char_length(delivery_id) <= 200),
    body_sha256    TEXT CHECK (body_sha256 IS NULL OR body_sha256 ~ '^[0-9a-f]{64}$'),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX intake_events_integration ON intake_events (integration_id, created_at DESC);
CREATE UNIQUE INDEX intake_events_delivery ON intake_events (integration_id, delivery_id) WHERE delivery_id IS NOT NULL;
CREATE UNIQUE INDEX intake_events_body ON intake_events (integration_id, body_sha256) WHERE body_sha256 IS NOT NULL;

-- Where a message to a task came from (WP06 messages; NULL = Queue UI/API).
ALTER TABLE task_messages ADD COLUMN via TEXT CHECK (via IS NULL OR via IN ('github', 'linear', 'slack', 'jira'));
