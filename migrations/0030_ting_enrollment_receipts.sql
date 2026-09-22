-- An explicit recipient enrollment may reactivate a Ting grant. Retain its
-- non-secret result so a retry cannot reactivate a later recipient revocation.
-- A reservation with no result is deliberately uncertain after a crash; it
-- must not be silently replayed as a new registration.
CREATE TABLE dm.ting_enrollment_receipts (
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    idempotency_key text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    response jsonb CHECK (response IS NULL OR jsonb_typeof(response) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    PRIMARY KEY (organization_id, actor_kind, actor_id, idempotency_key),
    CHECK ((response IS NULL) = (completed_at IS NULL))
);
