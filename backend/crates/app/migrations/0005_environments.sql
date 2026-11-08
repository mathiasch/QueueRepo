-- WP02: project environments (setup script, variables, network policy per
-- phase, dependency cache) and project secrets (setup phase only).

CREATE TABLE project_environments (
    project_id         UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    setup_script       TEXT NOT NULL DEFAULT '',
    setup_timeout_secs INT  NOT NULL DEFAULT 600
        CHECK (setup_timeout_secs BETWEEN 10 AND 3600),
    -- [{"name": "...", "value": "..."}] – not secret, visible to all members
    env_vars           JSONB NOT NULL DEFAULT '[]'
        CHECK (jsonb_typeof(env_vars) = 'array'),
    -- {"mode": "off|allowlist|open", "presets": [...], "domains": [...], "read_only": bool}
    setup_network      JSONB NOT NULL
        CHECK (setup_network->>'mode' IN ('off', 'allowlist', 'open')),
    agent_network      JSONB NOT NULL
        CHECK (agent_network->>'mode' IN ('off', 'allowlist', 'open')),
    cache_enabled      BOOLEAN NOT NULL DEFAULT TRUE,
    cache_files        TEXT[] NOT NULL DEFAULT '{}',
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE project_secrets (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name       TEXT NOT NULL CHECK (name ~ '^[A-Z_][A-Z0-9_]{0,63}$'),
    -- AES-256-GCM with MASTER_KEY (crypto.rs), never returned by the API
    value_enc  BYTEA NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, name)
);
