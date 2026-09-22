-- Only freshly IAM-verified application access tokens may enter this cache.
-- Rotating refresh tokens and single-use OBO proofs remain outside backend storage.
CREATE TABLE dm.ting_credentials (
    app_id text NOT NULL CHECK (octet_length(app_id) BETWEEN 1 AND 255),
    generation bigint NOT NULL CHECK (generation >= 0),
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    token_digest bytea NOT NULL CHECK (octet_length(token_digest)=32),
    nonce bytea NOT NULL CHECK (octet_length(nonce)=12),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) BETWEEN 17 AND 65552),
    session_id uuid,
    expires_at timestamptz NOT NULL,
    verified_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY(app_id,generation,organization_id,actor_kind,actor_id,token_digest),
    FOREIGN KEY(organization_id,actor_kind,actor_id)
        REFERENCES dm.actor_snapshots(organization_id,actor_kind,actor_id)
);
CREATE INDEX ting_credentials_expiry ON dm.ting_credentials(expires_at);
COMMENT ON TABLE dm.ting_credentials IS
    'At most eight encrypted IAM access-token candidates per app, generation, organization and typed actor; expiry never extends on reuse.';
