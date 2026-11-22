-- WP11: MFA, sessions, account lock, invitations, registration mode, quotas,
-- append-only audit log with hash chain, runner fleet.

-- Accounts ---------------------------------------------------------------------
ALTER TABLE users
    ADD COLUMN disabled_at         TIMESTAMPTZ,
    ADD COLUMN disabled_reason     TEXT NOT NULL DEFAULT '' CHECK (length(disabled_reason) <= 500),
    ADD COLUMN password_changed_at TIMESTAMPTZ;

-- Sessions become visible and revocable one by one.
ALTER TABLE sessions
    ADD COLUMN id           UUID NOT NULL DEFAULT gen_random_uuid(),
    ADD COLUMN last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN user_agent   TEXT NOT NULL DEFAULT '' CHECK (length(user_agent) <= 300),
    ADD COLUMN auth_method  TEXT NOT NULL DEFAULT 'password' CHECK (length(auth_method) <= 80);
CREATE UNIQUE INDEX sessions_id_idx ON sessions (id);
CREATE INDEX sessions_user_idx ON sessions (user_id);

-- MFA ----------------------------------------------------------------------------
-- TOTP seed encrypted with MASTER_KEY (AES-256-GCM). `last_step` = the RFC 6238
-- time step of the last accepted code (replay protection).
CREATE TABLE mfa_totp (
    user_id      UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    secret_enc   BYTEA NOT NULL,
    confirmed_at TIMESTAMPTZ,
    last_step    BIGINT NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE webauthn_credentials (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 60),
    cred_id      BYTEA NOT NULL UNIQUE,
    passkey      JSONB NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ
);
CREATE INDEX webauthn_credentials_user_idx ON webauthn_credentials (user_id);

-- Only HMAC-SHA-256 (pepper derived from MASTER_KEY); single use.
CREATE TABLE recovery_codes (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash  TEXT NOT NULL,
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, code_hash)
);

-- Short-lived server-side flows: the second login step, WebAuthn ceremonies.
CREATE TABLE auth_flows (
    id          UUID PRIMARY KEY,
    token_hash  TEXT NOT NULL UNIQUE,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('mfa_login', 'webauthn_register', 'webauthn_login')),
    method      TEXT NOT NULL DEFAULT '' CHECK (length(method) <= 80),
    state       JSONB,
    attempts    INT NOT NULL DEFAULT 0,
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX auth_flows_expiry_idx ON auth_flows (expires_at);

-- Brute-force throttle per key (`login:<email>`, `mfa:<user>`).
CREATE TABLE auth_throttle (
    key          TEXT PRIMARY KEY CHECK (length(key) <= 400),
    failures     INT NOT NULL DEFAULT 0,
    window_start TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_until TIMESTAMPTZ
);

-- Instance settings (one row) ----------------------------------------------------
CREATE TABLE instance_settings (
    id                INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    -- NULL = from the environment (ALLOW_REGISTRATION: open / closed).
    registration_mode TEXT CHECK (registration_mode IN ('open', 'invite', 'closed')),
    mfa_policy        TEXT NOT NULL DEFAULT 'off' CHECK (mfa_policy IN ('off', 'admins', 'all')),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO instance_settings (id) VALUES (1);

CREATE TABLE invitations (
    id          UUID PRIMARY KEY,
    token_hash  TEXT NOT NULL UNIQUE,
    email       TEXT CHECK (email IS NULL OR length(email) <= 320),
    make_admin  BOOLEAN NOT NULL DEFAULT FALSE,
    note        TEXT NOT NULL DEFAULT '' CHECK (length(note) <= 200),
    created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    used_at     TIMESTAMPTZ,
    used_by     UUID REFERENCES users(id) ON DELETE SET NULL,
    revoked_at  TIMESTAMPTZ
);
ALTER TABLE oauth_states ADD COLUMN invite_hash TEXT;

-- Quotas set by admins (bound to the WP13 cost accounting) -----------------------
CREATE TABLE user_quotas (
    user_id          UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    max_cost_30d_usd DOUBLE PRECISION CHECK (max_cost_30d_usd IS NULL OR max_cost_30d_usd >= 0),
    max_parallel     INT CHECK (max_parallel IS NULL OR max_parallel BETWEEN 1 AND 64),
    max_projects     INT CHECK (max_projects IS NULL OR max_projects BETWEEN 0 AND 10000),
    updated_by       UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Audit log: append-only, hash chain -----------------------------------------------
-- No foreign keys: entries outlive users and projects.
CREATE TABLE audit_log (
    id          BIGSERIAL PRIMARY KEY,
    seq         BIGINT NOT NULL UNIQUE,
    at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor_id    UUID,
    actor_label TEXT NOT NULL DEFAULT '' CHECK (length(actor_label) <= 320),
    action      TEXT NOT NULL CHECK (action ~ '^[a-z_]+(\.[a-z_]+)+$' AND length(action) <= 80),
    target_type TEXT NOT NULL DEFAULT '' CHECK (length(target_type) <= 40),
    target_id   TEXT NOT NULL DEFAULT '' CHECK (length(target_id) <= 200),
    project_id  UUID,
    details     JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(details) = 'object'),
    prev_hash   BYTEA NOT NULL,
    hash        BYTEA NOT NULL
);
CREATE INDEX audit_log_action_idx ON audit_log (action, seq);
CREATE INDEX audit_log_actor_idx ON audit_log (actor_id, seq);
CREATE INDEX audit_log_project_idx ON audit_log (project_id, seq);
CREATE INDEX audit_log_at_idx ON audit_log (at);

-- Canonical content of an entry (shared by the insert trigger and the check).
CREATE FUNCTION audit_entry_hash(p_prev BYTEA, p_seq BIGINT, p_at TIMESTAMPTZ, p_actor UUID,
                                 p_label TEXT, p_action TEXT, p_ttype TEXT, p_tid TEXT,
                                 p_project UUID, p_details JSONB)
RETURNS BYTEA LANGUAGE sql IMMUTABLE AS $$
    SELECT sha256(p_prev || convert_to(
        p_seq::text || '|' || to_char(p_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') || '|' ||
        coalesce(p_actor::text, '') || '|' || p_label || '|' || p_action || '|' ||
        p_ttype || '|' || p_tid || '|' || coalesce(p_project::text, '') || '|' || p_details::text,
        'UTF8'))
$$;

CREATE FUNCTION audit_log_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    last_seq  BIGINT;
    last_hash BYTEA;
BEGIN
    -- Serializes appends (held until commit): seq and chain stay gapless.
    PERFORM pg_advisory_xact_lock(7311001);
    SELECT seq, hash INTO last_seq, last_hash FROM audit_log ORDER BY seq DESC LIMIT 1;
    NEW.seq := coalesce(last_seq, 0) + 1;
    NEW.at := clock_timestamp();
    NEW.prev_hash := coalesce(last_hash, '\x'::bytea);
    NEW.hash := audit_entry_hash(NEW.prev_hash, NEW.seq, NEW.at, NEW.actor_id, NEW.actor_label,
                                 NEW.action, NEW.target_type, NEW.target_id, NEW.project_id, NEW.details);
    RETURN NEW;
END $$;

-- Retention: entries younger than 365 days can never be deleted, and only the
-- oldest entries (the chain stays verifiable from the first remaining one).
CREATE FUNCTION audit_log_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.at < now() - interval '365 days'
           AND NOT EXISTS (SELECT 1 FROM audit_log o WHERE o.seq < OLD.seq AND o.at >= now() - interval '365 days') THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'audit_log is append-only (entries younger than 365 days cannot be deleted)'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RAISE EXCEPTION 'audit_log is append-only (% not allowed)', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END $$;

CREATE FUNCTION audit_log_no_truncate() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_log is append-only (TRUNCATE not allowed)'
        USING ERRCODE = 'insufficient_privilege';
END $$;

CREATE TRIGGER audit_log_insert BEFORE INSERT ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_before_insert();
CREATE TRIGGER audit_log_update BEFORE UPDATE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_guard();
CREATE TRIGGER audit_log_delete BEFORE DELETE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_guard();
CREATE TRIGGER audit_log_truncate BEFORE TRUNCATE ON audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION audit_log_no_truncate();
REVOKE UPDATE, DELETE, TRUNCATE ON audit_log FROM PUBLIC;

-- First broken link of the chain (NULL = intact), checked in sequence order.
CREATE FUNCTION audit_log_verify(OUT checked BIGINT, OUT first_broken BIGINT)
LANGUAGE plpgsql STABLE AS $$
DECLARE
    r    RECORD;
    prev BYTEA := NULL;
BEGIN
    checked := 0;
    first_broken := NULL;
    FOR r IN SELECT * FROM audit_log ORDER BY seq LOOP
        IF (prev IS NOT NULL AND r.prev_hash <> prev)
           OR r.hash <> audit_entry_hash(r.prev_hash, r.seq, r.at, r.actor_id, r.actor_label, r.action,
                                         r.target_type, r.target_id, r.project_id, r.details) THEN
            first_broken := r.seq;
            RETURN;
        END IF;
        prev := r.hash;
        checked := checked + 1;
    END LOOP;
END $$;

-- Runner fleet -------------------------------------------------------------------
CREATE TABLE runners (
    id              UUID PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 60),
    -- NULL for the built-in runner (RUNNER_URL / RUNNER_TOKEN of the app).
    url             TEXT CHECK (url IS NULL OR length(url) <= 500),
    token_enc       BYTEA,
    builtin         BOOLEAN NOT NULL DEFAULT FALSE,
    status          TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'draining', 'revoked')),
    capacity        INT CHECK (capacity IS NULL OR capacity BETWEEN 1 AND 1000),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at    TIMESTAMPTZ,
    -- NULL = reachable (or not checked yet: the built-in runner starts usable).
    offline_since   TIMESTAMPTZ,
    engine          TEXT NOT NULL DEFAULT '',
    egress_enforced BOOLEAN NOT NULL DEFAULT FALSE,
    version         TEXT NOT NULL DEFAULT '',
    active_runs     INT NOT NULL DEFAULT 0,
    CHECK (builtin OR (url IS NOT NULL AND (token_enc IS NOT NULL OR status = 'revoked'))),
    CHECK (NOT builtin OR status <> 'revoked')
);
CREATE UNIQUE INDEX runners_one_builtin ON runners (builtin) WHERE builtin;
INSERT INTO runners (id, name, builtin) VALUES ('00000000-0000-0000-0000-000000000001', 'Standard-Runner', TRUE);

ALTER TABLE projects
    ADD COLUMN runner_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES runners(id);
CREATE INDEX projects_runner_idx ON projects (runner_id);
ALTER TABLE tasks ADD COLUMN runner_id UUID REFERENCES runners(id) ON DELETE SET NULL;
