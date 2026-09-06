-- Production control plane only. Secrets are authenticated-encrypted by the API.
CREATE TABLE dm.testing_environments (
    environment_id uuid PRIMARY KEY,
    organization_id text NOT NULL,
    creator_actor_id text NOT NULL,
    creator_actor_kind text NOT NULL CHECK (creator_actor_kind IN ('carbon', 'silicon')),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 128),
    description text CHECK (char_length(description) <= 4096),
    iam_environment_id uuid NOT NULL CHECK (iam_environment_id <> '00000000-0000-0000-0000-000000000000'),
    iam_app_id text NOT NULL,
    iam_environment_key_ciphertext text NOT NULL,
    iam_app_secret_ciphertext text NOT NULL,
    root_key_digest bytea UNIQUE,
    root_key_ciphertext text,
    status text NOT NULL CHECK (status IN ('creating', 'active', 'deleted', 'purging')),
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_activity_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    deleted_at timestamptz,
    purge_after timestamptz,
    CHECK ((status = 'active') = (root_key_digest IS NOT NULL AND root_key_ciphertext IS NOT NULL)),
    CHECK ((status IN ('deleted', 'purging')) = (deleted_at IS NOT NULL AND purge_after IS NOT NULL))
);
CREATE INDEX testing_environments_owner ON dm.testing_environments (organization_id, created_at DESC);
CREATE INDEX testing_environments_lifecycle ON dm.testing_environments (status, last_activity_at, purge_after);
