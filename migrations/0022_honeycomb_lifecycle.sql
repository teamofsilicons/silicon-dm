-- Production-only durable participant state. Tombstones prevent rediscovery after purge.
CREATE TABLE dm.honeycomb_environments (
    environment_id uuid PRIMARY KEY,
    organization_id text NOT NULL,
    app_id text NOT NULL,
    environment_revision bigint NOT NULL CHECK (environment_revision > 0),
    generation bigint NOT NULL CHECK (generation > 0),
    key_version bigint NOT NULL CHECK (key_version > 0),
    operation_id uuid NOT NULL,
    state text NOT NULL CHECK (state IN ('pending', 'active', 'disabled', 'retired', 'purged')),
    testing_key_ciphertext text NOT NULL,
    last_activity_at timestamptz,
    activity_reported_at timestamptz
);
CREATE TABLE dm.honeycomb_operations (
    environment_id uuid NOT NULL REFERENCES dm.honeycomb_environments(environment_id),
    operation_id uuid NOT NULL,
    request_hash bytea NOT NULL,
    receipt jsonb NOT NULL,
    PRIMARY KEY(environment_id, operation_id)
);
