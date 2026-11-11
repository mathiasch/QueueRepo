-- WP06: plan mode with approval, steering messages ("Nachricht an den
-- Agenten", "Weiter mit …") and inline diff comments.

ALTER TABLE projects
    -- New tasks plan first (default of the composer option).
    ADD COLUMN plan_mode              BOOLEAN NOT NULL DEFAULT FALSE,
    -- Plans are approved automatically after this many minutes (NULL = never).
    ADD COLUMN plan_auto_approve_mins INT CHECK (plan_auto_approve_mins BETWEEN 5 AND 10080);

ALTER TABLE tasks
    ADD COLUMN plan_mode     BOOLEAN NOT NULL DEFAULT FALSE,
    -- NULL = no plan (yet); see queued_api::steer::PlanState.
    ADD COLUMN plan_state    TEXT CHECK (plan_state IN ('planning', 'ready', 'approved', 'rejected')),
    ADD COLUMN plan_revision INT NOT NULL DEFAULT 0 CHECK (plan_revision BETWEEN 0 AND 100),
    -- Revision note of a human for the next plan run.
    ADD COLUMN plan_feedback TEXT CHECK (plan_feedback IS NULL OR char_length(plan_feedback) <= 4000),
    -- Questions, attempted changes, source, approval (queued_api::steer::PlanMeta).
    ADD COLUMN plan_meta     JSONB,
    -- CLI session of the last agent run: {provider, id}.
    ADD COLUMN agent_session JSONB,
    -- "Weiter mit …": the task whose session this one continues.
    ADD COLUMN session_from  UUID REFERENCES tasks(id) ON DELETE SET NULL,
    ADD COLUMN steer_rounds  INT NOT NULL DEFAULT 0 CHECK (steer_rounds >= 0);
-- New kind value 'continue' (no CHECK on tasks.kind).

CREATE TABLE task_messages (
    id           UUID PRIMARY KEY,
    task_id      UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    author_id    UUID REFERENCES users(id) ON DELETE SET NULL,
    body         TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 60000),
    source       TEXT NOT NULL DEFAULT 'message' CHECK (source IN ('message', 'comments')),
    -- queued → delivered | withdrawn | dropped; every change is a single
    -- conditional UPDATE (no race between withdrawing and delivering).
    state        TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'delivered', 'withdrawn', 'dropped')),
    -- Interrupt the running agent ("Sofort zustellen").
    urgent       BOOLEAN NOT NULL DEFAULT FALSE,
    delivery     TEXT CHECK (delivery IN ('prompt', 'resume', 'handoff', 'continuation')),
    -- The "Weiter mit …" task that got it.
    continued_as UUID REFERENCES tasks(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ,
    withdrawn_at TIMESTAMPTZ,
    CHECK ((state = 'delivered') = (delivered_at IS NOT NULL))
);
CREATE INDEX task_messages_task ON task_messages (task_id, state, created_at);

CREATE TABLE diff_comments (
    id         UUID PRIMARY KEY,
    task_id    UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    author_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    path       TEXT NOT NULL CHECK (char_length(path) BETWEEN 1 AND 1000),
    side       TEXT NOT NULL CHECK (side IN ('new', 'old')),
    line_start INT NOT NULL CHECK (line_start >= 1),
    line_end   INT NOT NULL CHECK (line_end >= line_start AND line_end - line_start < 200),
    body       TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 2000),
    -- The commented lines as they were (JSON array of strings), for
    -- relocating and for the "veraltet" mark when the diff changes.
    anchor     JSONB NOT NULL,
    -- Commit of the diff the comment was made on (tasks.snapshot_after).
    diff_rev   TEXT NOT NULL,
    state      TEXT NOT NULL DEFAULT 'draft' CHECK (state IN ('draft', 'sent')),
    message_id UUID REFERENCES task_messages(id) ON DELETE SET NULL,
    sent_task  UUID REFERENCES tasks(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at    TIMESTAMPTZ
);
CREATE INDEX diff_comments_task ON diff_comments (task_id, author_id, state);
