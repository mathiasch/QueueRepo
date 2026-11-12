-- WP09: managed project rules (AGENTS.md) with versions and proposals,
-- the structured project state with revisions, task attachments and the
-- OnEmpty dedupe/critic settings.

-- Project rules. `version` is set when a human approves (the newest
-- approved version is what every prompt gets); proposals and rejected
-- proposals have no version.
CREATE TABLE knowledge_versions (
    id           UUID PRIMARY KEY,
    project_id   UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    version      INT CHECK (version IS NULL OR version >= 1),
    status       TEXT NOT NULL CHECK (status IN ('proposed', 'approved', 'rejected')),
    content      TEXT NOT NULL CHECK (octet_length(content) <= 32768),
    -- human: written in the UI; agent: `.queue/knowledge.md` of a task;
    -- repo: imported from AGENTS.md/CLAUDE.md/GEMINI.md of the repository.
    source       TEXT NOT NULL CHECK (source IN ('human', 'agent', 'repo')),
    author_id    UUID REFERENCES users(id) ON DELETE SET NULL,
    task_id      UUID REFERENCES tasks(id) ON DELETE SET NULL,
    -- Approved version the proposal started from.
    base_version INT,
    note         TEXT NOT NULL DEFAULT '' CHECK (char_length(note) <= 2000),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    decided_at   TIMESTAMPTZ,
    CHECK ((status = 'approved') = (version IS NOT NULL)),
    UNIQUE (project_id, version)
);
CREATE INDEX knowledge_open ON knowledge_versions (project_id, created_at) WHERE status = 'proposed';

-- Structured project state (queued_common::state, schema v1): one row per
-- revision, the newest is current.
CREATE TABLE project_state_revisions (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    rev        INT NOT NULL CHECK (rev >= 1),
    data       JSONB NOT NULL CHECK (octet_length(data::TEXT) <= 131072),
    source     TEXT NOT NULL CHECK (source IN ('human', 'agent')),
    task_id    UUID REFERENCES tasks(id) ON DELETE SET NULL,
    author_id  UUID REFERENCES users(id) ON DELETE SET NULL,
    note       TEXT NOT NULL DEFAULT '' CHECK (char_length(note) <= 4000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, rev)
);

ALTER TABLE tasks
    -- Revision of the project state the task's run started with (0 = none).
    ADD COLUMN state_rev INT,
    -- Checked state of the run, applied when the task is done.
    ADD COLUMN state_proposal JSONB,
    -- OnEmpty: dedupe/critic verdict of the proposal that created this task.
    ADD COLUMN dedupe JSONB,
    -- OnEmpty task: report of all proposals and their verdicts.
    ADD COLUMN proposals JSONB,
    -- `git revert` of the task's changes (negative example for OnEmpty).
    ADD COLUMN reverted_at TIMESTAMPTZ;

-- Attachments. Draft = no task yet (uploaded in the composer); drafts are
-- removed after a day. Deleting a task or project removes its attachments.
CREATE TABLE attachments (
    id           UUID PRIMARY KEY,
    project_id   UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    task_id      UUID REFERENCES tasks(id) ON DELETE CASCADE,
    -- Uploader (quota).
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Name in `.queue/attachments/` (sanitized, unique per task).
    name         TEXT NOT NULL CHECK (name ~ '^[a-z0-9][a-z0-9._-]{0,79}$' AND name NOT LIKE '%..%'),
    display_name TEXT NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 120),
    mime         TEXT NOT NULL CHECK (mime IN ('image/png', 'image/jpeg', 'application/pdf', 'text/plain')),
    size         INT NOT NULL CHECK (size BETWEEN 1 AND 10485760),
    sha256       TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    width        INT,
    height       INT,
    data         BYTEA NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (octet_length(data) = size)
);
CREATE INDEX attachments_task ON attachments (task_id, created_at) WHERE task_id IS NOT NULL;
CREATE INDEX attachments_user ON attachments (user_id);
CREATE INDEX attachments_drafts ON attachments (created_at) WHERE task_id IS NULL;
CREATE UNIQUE INDEX attachments_task_name ON attachments (task_id, name) WHERE task_id IS NOT NULL;

ALTER TABLE user_settings
    -- Similarity from which an OnEmpty proposal is dropped as a duplicate.
    ADD COLUMN dedupe_threshold REAL NOT NULL DEFAULT 0.6 CHECK (dedupe_threshold > 0 AND dedupe_threshold <= 1),
    -- Similarity from which a proposal needs a human approval.
    ADD COLUMN dedupe_review_threshold REAL NOT NULL DEFAULT 0.35 CHECK (dedupe_review_threshold > 0 AND dedupe_review_threshold <= 1),
    -- Finished tasks compared against (days).
    ADD COLUMN dedupe_window_days INT NOT NULL DEFAULT 90 CHECK (dedupe_window_days BETWEEN 1 AND 3650),
    -- Second opinion by a model (direct access of the analysis step).
    ADD COLUMN critic_llm BOOLEAN NOT NULL DEFAULT FALSE,
    ADD CHECK (dedupe_review_threshold <= dedupe_threshold);

-- The optional model critic of OnEmpty proposals is a cost step of its own.
ALTER TABLE usage_log DROP CONSTRAINT usage_log_step_check;
ALTER TABLE usage_log ADD CONSTRAINT usage_log_step_check
    CHECK (step IN ('execute', 'plan', 'review', 'analyze', 'critic'));
