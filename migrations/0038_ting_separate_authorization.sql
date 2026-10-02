-- Login scopes and ordinary access-token caches cannot become delegated grants.
DELETE FROM dm.ting_credentials;
DELETE FROM dm.ting_automatic_enrollments;
CREATE TABLE dm.ting_authorizations (
 app_id text NOT NULL, generation bigint NOT NULL CHECK(generation>=0),
 organization_id text NOT NULL, actor_kind dm.actor_kind NOT NULL, actor_id text NOT NULL,
 authorization_id uuid NOT NULL, expires_at timestamptz NOT NULL,
 start_key text NOT NULL, start_nonce bytea NOT NULL CHECK(octet_length(start_nonce)=12),
 start_ciphertext bytea NOT NULL CHECK(octet_length(start_ciphertext) BETWEEN 17 AND 131072),
 UNIQUE(app_id,generation,organization_id,actor_kind,actor_id,start_key),
 completion_digest bytea, completed_at timestamptz,
 PRIMARY KEY(app_id,generation,organization_id,actor_kind,actor_id,authorization_id)
);
CREATE TABLE dm.ting_obo_credentials (
 app_id text NOT NULL, generation bigint NOT NULL CHECK(generation>=0),
 organization_id text NOT NULL, actor_kind dm.actor_kind NOT NULL, actor_id text NOT NULL,
 endpoint_id text NOT NULL CHECK(endpoint_id IN ('subscriptions.register','tings.send')),
 grant_id uuid NOT NULL, nonce bytea NOT NULL CHECK(octet_length(nonce)=12),
 ciphertext bytea NOT NULL CHECK(octet_length(ciphertext) BETWEEN 17 AND 131072),
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(app_id,generation,organization_id,actor_kind,actor_id,endpoint_id)
);
COMMENT ON TABLE dm.ting_obo_credentials IS 'Dedicated, separately approved Ting access/refresh credentials; encrypted and bound to app, actor, org, environment generation and endpoint. No login credential migration.';
