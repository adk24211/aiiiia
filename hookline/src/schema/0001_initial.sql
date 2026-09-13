-- Tenants. One application per customer of yours, or per project.
CREATE TABLE apps (
    id          TEXT NOT NULL PRIMARY KEY,
    name        TEXT NOT NULL,
    -- Your own identifier for this tenant, so you can address it by the id you
    -- already have instead of storing a mapping.
    uid         TEXT UNIQUE,
    metadata    TEXT NOT NULL DEFAULT '{}',
    created_at  INTEGER NOT NULL
);

-- A URL belonging to an application, with its own secrets and filters.
CREATE TABLE endpoints (
    id              TEXT NOT NULL PRIMARY KEY,
    app_id          TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    url             TEXT NOT NULL,
    description     TEXT NOT NULL DEFAULT '',
    -- JSON array of event types, or NULL for every type.
    event_types     TEXT,
    disabled_at     INTEGER,
    disabled_reason TEXT,
    rate_limit      INTEGER,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX endpoints_by_app ON endpoints(app_id, id);

-- An endpoint has more than one secret during a rotation: outgoing requests
-- are signed with all of them, so the consumer can move at their own pace.
CREATE TABLE endpoint_secrets (
    id          TEXT NOT NULL PRIMARY KEY,
    endpoint_id TEXT NOT NULL REFERENCES endpoints(id) ON DELETE CASCADE,
    secret      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER
);
CREATE INDEX secrets_by_endpoint ON endpoint_secrets(endpoint_id, created_at);

-- An event you sent. One message fans out to every matching endpoint.
CREATE TABLE messages (
    id              TEXT NOT NULL PRIMARY KEY,
    app_id          TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    event_type      TEXT NOT NULL,
    payload         TEXT NOT NULL,
    idempotency_key TEXT,
    created_at      INTEGER NOT NULL
);
-- Partial, so the many messages without a key do not collide with each other.
CREATE UNIQUE INDEX messages_idempotency
    ON messages(app_id, idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX messages_by_app ON messages(app_id, id);

-- The queue. One row per (message, endpoint): the unit that is retried,
-- succeeds, or is given up on.
CREATE TABLE deliveries (
    id          TEXT NOT NULL PRIMARY KEY,
    message_id  TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    endpoint_id TEXT NOT NULL REFERENCES endpoints(id) ON DELETE CASCADE,
    app_id      TEXT NOT NULL,
    -- pending | succeeded | failed | cancelled
    status      TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    next_at     INTEGER NOT NULL,
    -- Held by a worker until this time. A worker that dies leaves a lease that
    -- simply expires, and the delivery is picked up again.
    lease_until INTEGER,
    last_error  TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
-- The index the workers poll. Partial on `pending` so it stays small however
-- large the history grows.
CREATE INDEX deliveries_ready ON deliveries(next_at) WHERE status = 'pending';
CREATE INDEX deliveries_by_message ON deliveries(message_id);
CREATE INDEX deliveries_by_endpoint ON deliveries(endpoint_id, id);

-- Every HTTP request made, successful or not. This is the audit trail, and
-- the thing a support question is answered from.
CREATE TABLE attempts (
    id               TEXT NOT NULL PRIMARY KEY,
    delivery_id      TEXT NOT NULL REFERENCES deliveries(id) ON DELETE CASCADE,
    endpoint_id      TEXT NOT NULL,
    message_id       TEXT NOT NULL,
    app_id           TEXT NOT NULL,
    attempt_no       INTEGER NOT NULL,
    -- success | failure
    status           TEXT NOT NULL,
    status_code      INTEGER,
    error            TEXT,
    duration_ms      INTEGER NOT NULL,
    response_snippet TEXT,
    created_at       INTEGER NOT NULL
);
CREATE INDEX attempts_by_delivery ON attempts(delivery_id, attempt_no);
CREATE INDEX attempts_by_endpoint ON attempts(endpoint_id, id);
CREATE INDEX attempts_by_app ON attempts(app_id, id);

-- Enough state to run a circuit breaker and a rate limiter without scanning
-- the attempt log for either.
CREATE TABLE endpoint_health (
    endpoint_id          TEXT NOT NULL PRIMARY KEY REFERENCES endpoints(id) ON DELETE CASCADE,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    circuit_open_until   INTEGER,
    last_success_at      INTEGER,
    last_failure_at      INTEGER,
    -- A rate-limited endpoint may be sent to again at this time. One
    -- timestamp is the whole limiter: claiming a delivery moves it forward by
    -- the spacing the limit implies, so the rate is exact and enforcing it
    -- costs no counting.
    next_allowed_at      INTEGER
);

-- API credentials. Only the hash is stored; the token is shown once.
CREATE TABLE api_keys (
    id           TEXT NOT NULL PRIMARY KEY,
    name         TEXT NOT NULL,
    hash         TEXT NOT NULL UNIQUE,
    prefix       TEXT NOT NULL,
    scope        TEXT NOT NULL DEFAULT 'admin',
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    revoked_at   INTEGER
);
