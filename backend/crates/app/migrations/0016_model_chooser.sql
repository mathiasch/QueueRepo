-- WP15: model chooser (task analysis, decision, learning, transparency).

-- Chooser settings per project (absent row = chooser off, defaults).
CREATE TABLE chooser_settings (
    project_id          UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    enabled             BOOLEAN NOT NULL DEFAULT FALSE,
    -- 0 = cheapest, 100 = quality first.
    quality             SMALLINT NOT NULL DEFAULT 50 CHECK (quality BETWEEN 0 AND 100),
    -- Exploration budget per 30 days (USD).
    explore_budget_usd  DOUBLE PRECISION NOT NULL DEFAULT 2 CHECK (explore_budget_usd BETWEEN 0 AND 1000),
    -- Refine the heuristic with a cheap model (cost booked as step 'classify').
    classify            BOOLEAN NOT NULL DEFAULT FALSE,
    blocked_models      TEXT[] NOT NULL DEFAULT '{}' CHECK (cardinality(blocked_models) <= 50),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A user's own chooser choices. Learning data flows into the global priors
-- only with share_learning AND the instance setting chooser_pooling.
CREATE TABLE chooser_user_prefs (
    user_id         UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    share_learning  BOOLEAN NOT NULL DEFAULT FALSE,
    blocked_models  TEXT[] NOT NULL DEFAULT '{}' CHECK (cardinality(blocked_models) <= 50),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Outcomes the chooser learns from (one per task, step and source; the
-- latest feedback replaces the previous one).
CREATE TABLE chooser_observations (
    id          BIGSERIAL PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id  UUID REFERENCES projects(id) ON DELETE SET NULL,
    task_id     UUID REFERENCES tasks(id) ON DELETE SET NULL,
    step        TEXT NOT NULL CHECK (step IN ('plan', 'execute', 'review', 'analyze')),
    category    TEXT NOT NULL CHECK (category IN ('bugfix', 'feature', 'refactor', 'docs', 'tests',
                                                  'research', 'ui', 'analysis', 'other')),
    -- Price catalog id of the model (never free text).
    model_key   TEXT NOT NULL CHECK (length(model_key) <= 120),
    family      TEXT NOT NULL CHECK (length(family) <= 40),
    size        TEXT NOT NULL DEFAULT 'M' CHECK (size IN ('S', 'M', 'L')),
    success     REAL NOT NULL CHECK (success BETWEEN 0 AND 1),
    weight      REAL NOT NULL DEFAULT 1 CHECK (weight > 0 AND weight <= 5),
    cost_usd    DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (cost_usd >= 0),
    source      TEXT NOT NULL CHECK (source IN ('run', 'feedback', 'revert', 'eval')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (task_id, step, source)
);
CREATE INDEX chooser_observations_user_idx ON chooser_observations (user_id, step, category);
CREATE INDEX chooser_observations_cell_idx ON chooser_observations (step, category, model_key);

-- One row per chooser decision (statistics, exploration budget).
CREATE TABLE chooser_decisions (
    task_id            UUID PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    user_id            UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id         UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source             TEXT NOT NULL CHECK (source IN ('chooser', 'fallback', 'override')),
    category           TEXT NOT NULL,
    model_key          TEXT,
    explored           BOOLEAN NOT NULL DEFAULT FALSE,
    explore_cost_usd   DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (explore_cost_usd >= 0),
    expected_cost_usd  DOUBLE PRECISION NOT NULL DEFAULT 0,
    classify_cost_usd  DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (classify_cost_usd >= 0),
    fallback_reason    TEXT CHECK (fallback_reason IS NULL OR length(fallback_reason) <= 500),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX chooser_decisions_project_idx ON chooser_decisions (project_id, created_at);
CREATE INDEX chooser_decisions_created_idx ON chooser_decisions (created_at);

-- Instance-wide learning settings (admin).
ALTER TABLE instance_settings
    ADD COLUMN chooser_pooling BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN chooser_half_life_days INT NOT NULL DEFAULT 60
        CHECK (chooser_half_life_days BETWEEN 7 AND 365);

-- The classification call is a cost step of its own.
ALTER TABLE usage_log DROP CONSTRAINT usage_log_step_check;
ALTER TABLE usage_log ADD CONSTRAINT usage_log_step_check
    CHECK (step IN ('execute', 'plan', 'review', 'analyze', 'critic', 'select', 'classify'));
