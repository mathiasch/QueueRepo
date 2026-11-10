-- WP05: output mode per project, git host adapters, pull requests and the
-- feedback loop (CI results and review comments → follow-up tasks).

ALTER TABLE projects
    -- commit = merge queue onto the target branch (WP04, default);
    -- branch = push the task branch; pr = push + pull/merge request.
    ADD COLUMN output_mode      TEXT NOT NULL DEFAULT 'commit' CHECK (output_mode IN ('commit', 'branch', 'pr')),
    -- NULL = detect from the remote URL.
    ADD COLUMN git_host         TEXT CHECK (git_host IN ('github', 'gitlab', 'gitea')),
    -- Overrides of the REST base and of `owner/repo` (self-hosted, tests).
    ADD COLUMN git_api_url      TEXT,
    ADD COLUMN git_repo_path    TEXT,
    ADD COLUMN pr_draft         BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN pr_labels        TEXT[] NOT NULL DEFAULT '{}',
    ADD COLUMN pr_auto_fix      BOOLEAN NOT NULL DEFAULT TRUE,
    -- Loop protection: follow-up tasks per pull request.
    ADD COLUMN pr_max_followups INT NOT NULL DEFAULT 3 CHECK (pr_max_followups BETWEEN 0 AND 10);

CREATE TABLE pull_requests (
    id             UUID PRIMARY KEY,
    project_id     UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- Task whose branch the request was opened for.
    task_id        UUID NOT NULL UNIQUE REFERENCES tasks(id) ON DELETE CASCADE,
    user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    host           TEXT NOT NULL CHECK (host IN ('github', 'gitlab', 'gitea')),
    head_branch    TEXT NOT NULL CHECK (head_branch ~ '^queue/[0-9a-f]{12,32}$'),
    base_branch    TEXT NOT NULL,
    title          TEXT NOT NULL,
    -- Number on the host (GitHub/Gitea number, GitLab iid); NULL while creating.
    number         BIGINT,
    url            TEXT CHECK (url IS NULL OR url ~ '^https?://'),
    state          TEXT NOT NULL DEFAULT 'creating' CHECK (state IN ('creating', 'open', 'merged', 'closed')),
    draft          BOOLEAN NOT NULL DEFAULT FALSE,
    head_sha       TEXT,
    ci_state       TEXT NOT NULL DEFAULT 'none' CHECK (ci_state IN ('none', 'pending', 'success', 'failure')),
    checks         JSONB NOT NULL DEFAULT '[]',
    conflict       BOOLEAN NOT NULL DEFAULT FALSE,
    auto_fix       BOOLEAN NOT NULL DEFAULT TRUE,
    followups      INT NOT NULL DEFAULT 0 CHECK (followups >= 0),
    max_followups  INT NOT NULL CHECK (max_followups BETWEEN 0 AND 20),
    -- Account of the token (its own comments never trigger anything).
    bot_login      TEXT,
    bot_id         BIGINT,
    -- If-None-Match per request URL (only stored after the response was processed).
    etags          JSONB NOT NULL DEFAULT '{}',
    failures       INT NOT NULL DEFAULT 0,
    -- The host asked to wait until then (Retry-After / used-up quota):
    -- neither "check now" nor a webhook may call it earlier.
    rate_until     TIMESTAMPTZ,
    next_poll_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A poller works on the row until then (crash ⇒ expires, recover() clears).
    lease_until    TIMESTAMPTZ,
    last_polled_at TIMESTAMPTZ,
    last_error     TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, head_branch)
);
CREATE INDEX pull_requests_poll_idx ON pull_requests (next_poll_at) WHERE state IN ('creating', 'open');

-- Every piece of feedback Queue looked at, exactly once (dedupe per comment
-- and per CI run = head commit). Never the text itself.
CREATE TABLE pr_feedback (
    id          BIGSERIAL PRIMARY KEY,
    pr_id       UUID NOT NULL REFERENCES pull_requests(id) ON DELETE CASCADE,
    source      TEXT NOT NULL CHECK (source IN ('ci', 'comment', 'review', 'review_comment')),
    external_id TEXT NOT NULL,
    author      TEXT,
    outcome     TEXT NOT NULL CHECK (outcome IN ('task', 'ignored_permission', 'ignored_bot', 'ignored_untriggered', 'ignored_limit', 'ignored_disabled')),
    task_id     UUID REFERENCES tasks(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pr_id, source, external_id)
);

-- Follow-up tasks (and their fixes) work on and push to this request's branch.
ALTER TABLE tasks ADD COLUMN pr_id UUID REFERENCES pull_requests(id) ON DELETE SET NULL;
CREATE INDEX tasks_pr_idx ON tasks (pr_id) WHERE pr_id IS NOT NULL;

-- New merge state: the task branch was pushed (output branch/pr).
ALTER TABLE tasks DROP CONSTRAINT tasks_merge_state_check;
ALTER TABLE tasks ADD CONSTRAINT tasks_merge_state_check
    CHECK (merge_state IN ('queued', 'merging', 'merged', 'conflict', 'failed', 'published'));
