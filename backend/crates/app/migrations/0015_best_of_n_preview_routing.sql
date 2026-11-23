-- WP12: Best-of-N, preview and routing decisions.

-- Best-of-N: a task with attempts > 1 is a group; its attempts are tasks of
-- kind 'attempt' (own working tree, branch and container each). New status
-- values 'comparing' (group) and 'candidate' (successful attempt waiting for
-- the selection) – no CHECK on tasks.status, as before.
ALTER TABLE tasks
    ADD COLUMN attempts INT NOT NULL DEFAULT 1 CHECK (attempts BETWEEN 1 AND 8),
    ADD COLUMN attempt_of UUID REFERENCES tasks(id) ON DELETE CASCADE,
    ADD COLUMN attempt_no INT CHECK (attempt_no BETWEEN 1 AND 8),
    -- Group: targets, automatic selection, choice, budget abort (JSON).
    ADD COLUMN bon JSONB,
    -- Lease of a running automatic selection (restart-safe).
    ADD COLUMN bon_select_at TIMESTAMPTZ,
    -- Why the agent loop runs on this access/model (rule, escalation, …).
    ADD COLUMN routing_decision JSONB,
    ADD CONSTRAINT tasks_attempt_pair CHECK ((attempt_of IS NULL) = (attempt_no IS NULL)),
    ADD CONSTRAINT tasks_attempt_not_group CHECK (attempt_of IS NULL OR attempts = 1);
CREATE INDEX tasks_attempt_of_idx ON tasks (attempt_of) WHERE attempt_of IS NOT NULL;

-- The automatic selection is a cost step of its own.
ALTER TABLE usage_log DROP CONSTRAINT usage_log_step_check;
ALTER TABLE usage_log ADD CONSTRAINT usage_log_step_check
    CHECK (step IN ('execute', 'plan', 'review', 'analyze', 'critic', 'select'));

-- Preview settings per project (dev server in the task's sandbox).
CREATE TABLE preview_settings (
    project_id          UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    enabled             BOOLEAN NOT NULL DEFAULT FALSE,
    command             TEXT NOT NULL DEFAULT '' CHECK (length(command) <= 2000),
    port                INT NOT NULL DEFAULT 3000 CHECK (port BETWEEN 1024 AND 65535),
    ready_path          TEXT NOT NULL DEFAULT '/' CHECK (ready_path LIKE '/%' AND length(ready_path) <= 300),
    ready_timeout_secs  INT NOT NULL DEFAULT 60 CHECK (ready_timeout_secs BETWEEN 5 AND 600),
    max_lifetime_secs   INT NOT NULL DEFAULT 900 CHECK (max_lifetime_secs BETWEEN 60 AND 3600),
    screenshots         BOOLEAN NOT NULL DEFAULT TRUE,
    screenshot_paths    TEXT[] NOT NULL DEFAULT ARRAY['/'] CHECK (cardinality(screenshot_paths) <= 5),
    ui_globs            TEXT[] NOT NULL DEFAULT ARRAY['**/*.html','**/*.css','**/*.scss','**/*.js','**/*.jsx','**/*.ts','**/*.tsx','**/*.vue','**/*.svelte']
                        CHECK (cardinality(ui_globs) <= 30),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Live previews. The capability (subdomain of PREVIEW_ORIGIN) is looked up
-- by its SHA-256 and kept encrypted (master key) to show the link to
-- other project members.
CREATE TABLE previews (
    id           UUID PRIMARY KEY,
    project_id   UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    task_id      UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    started_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    cap_hash     BYTEA NOT NULL UNIQUE CHECK (length(cap_hash) = 32),
    cap_enc      BYTEA NOT NULL,
    -- Run name at the runner.
    runner_name  TEXT NOT NULL,
    runner_id    UUID,
    status       TEXT NOT NULL CHECK (status IN ('starting', 'ready', 'stopped', 'failed')),
    error        TEXT,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL,
    stopped_at   TIMESTAMPTZ
);
CREATE INDEX previews_task_idx ON previews (task_id, started_at DESC);
CREATE INDEX previews_active_idx ON previews (expires_at) WHERE status IN ('starting', 'ready');

-- Screenshots of the dev server after a run (PNG, at most 8 MB each).
CREATE TABLE task_screenshots (
    id          UUID PRIMARY KEY,
    task_id     UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    viewport    TEXT NOT NULL CHECK (viewport IN ('desktop', 'mobile')),
    path        TEXT NOT NULL CHECK (length(path) <= 300),
    png         BYTEA NOT NULL CHECK (length(png) <= 8388608),
    width       INT NOT NULL,
    height      INT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX task_screenshots_task_idx ON task_screenshots (task_id, created_at);
