-- Root-key responses are encrypted with the testing control-plane encryption key.
-- Pending records retain the planned environment and key across process failure.
CREATE TABLE dm.testing_mutations (
    mutation_id uuid PRIMARY KEY,
    organization_id text NOT NULL,
    actor_kind text NOT NULL,
    actor_id text NOT NULL,
    operation text NOT NULL,
    idempotency_key text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    environment_id uuid NOT NULL,
    planned_key_ciphertext text NOT NULL,
    response_ciphertext text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    UNIQUE(organization_id,actor_kind,actor_id,operation,idempotency_key)
);
CREATE INDEX testing_mutations_retention ON dm.testing_mutations(completed_at);
