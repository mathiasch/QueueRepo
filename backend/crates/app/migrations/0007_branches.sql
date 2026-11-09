-- WP04: branch/working tree per task, parallelism per project, merge queue.

-- Tasks of one project that may run at the same time (each in its own
-- working tree on its own branch).
ALTER TABLE projects
    ADD COLUMN parallelism INT NOT NULL DEFAULT 1 CHECK (parallelism BETWEEN 1 AND 8);

ALTER TABLE tasks
    -- Task branch `queue/<12..32 hex>`; NULL = task from before WP04
    -- (worked directly on the target branch).
    ADD COLUMN branch          TEXT CHECK (branch IS NULL OR branch ~ '^queue/[0-9a-f]{12,32}$'),
    -- Target-branch commit the branch is based on; after the merge the
    -- parent of the merged commit (diff/revert range: merge_base..snapshot_after).
    ADD COLUMN merge_base      TEXT,
    -- Merge queue: queued → merging → merged | conflict | failed.
    ADD COLUMN merge_state     TEXT CHECK (merge_state IN ('queued', 'merging', 'merged', 'conflict', 'failed')),
    ADD COLUMN merge_queued_at TIMESTAMPTZ,
    -- Fix task that continues the (unmerged) changes of this task.
    ADD COLUMN carry_from      UUID REFERENCES tasks(id) ON DELETE SET NULL,
    -- Working tree (and, if merged/discarded, the branch) removed by the runner.
    ADD COLUMN workspace_cleaned BOOLEAN NOT NULL DEFAULT FALSE;

CREATE UNIQUE INDEX tasks_branch_idx ON tasks (project_id, branch) WHERE branch IS NOT NULL;
CREATE INDEX tasks_merge_queue_idx ON tasks (project_id, merge_queued_at) WHERE status = 'merging';
CREATE INDEX tasks_running_project_idx ON tasks (project_id) WHERE status = 'running';
-- New status 'merging' (no CHECK on tasks.status).
