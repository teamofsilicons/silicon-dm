-- Silicon Accounts identity for DM 0.15. Additive only: no IAM-era table, column or row
-- is dropped or rewritten here. Accounts uuids are short, case-sensitive text such as
-- `zQo` (never RFC 4122 UUIDs): they are stored as text and compared exactly.
--
-- The operator command `dm-migrate link-identities` re-keys IAM-era actor ids
-- (`c:handle`, `si:handle`) to Accounts uuids at cutover and records every pair in
-- dm.identity_links. Behaviour that depends on these tables is in 0040.

-- Every conversation created from DM 0.15 on lives in this storage partition. IAM
-- organization handles never contain `@`, so it cannot collide with a legacy partition.
INSERT INTO dm.organization_snapshots (organization_id, status, iam_version)
VALUES ('@accounts', 'active', 1)
ON CONFLICT (organization_id) DO NOTHING;

-- Local cache of the Silicon Accounts accounts DM has seen, keyed by the permanent uuid.
-- Filled from verified access tokens, app lookups and signed webhooks; never authoritative.
CREATE TABLE dm.accounts (
    uuid text COLLATE "C" PRIMARY KEY,
    kind dm.actor_kind NOT NULL,
    current_id text,
    display_name text,
    pfp_url text,
    custodian_uuid text COLLATE "C",
    status text NOT NULL DEFAULT 'active',
    profile_version bigint NOT NULL DEFAULT 0,
    id_changed_at timestamptz,
    custodian_changed_at timestamptz,
    revoked_before timestamptz,
    last_authenticated_at timestamptz,
    refreshed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    deleted_at timestamptz,
    CONSTRAINT accounts_uuid_format CHECK (uuid ~ '^[A-Za-z0-9]{3,64}$'),
    CONSTRAINT accounts_current_id_format
        CHECK (current_id IS NULL OR current_id ~ '^(c|si):[a-z0-9_-]{1,50}$'),
    CONSTRAINT accounts_kind_matches_id CHECK (
        current_id IS NULL OR (kind = 'carbon') = (current_id LIKE 'c:%')
    ),
    CONSTRAINT accounts_custodian_only_for_silicons
        CHECK (custodian_uuid IS NULL OR kind = 'silicon'),
    CONSTRAINT accounts_custodian_format
        CHECK (custodian_uuid IS NULL OR custodian_uuid ~ '^[A-Za-z0-9]{3,64}$'),
    CONSTRAINT accounts_status CHECK (status IN ('active', 'deleted')),
    CONSTRAINT accounts_deleted_consistency
        CHECK ((status = 'deleted') = (deleted_at IS NOT NULL)),
    CONSTRAINT accounts_display_name_length
        CHECK (display_name IS NULL OR char_length(display_name) <= 200),
    CONSTRAINT accounts_pfp_url_length CHECK (pfp_url IS NULL OR char_length(pfp_url) <= 2048),
    CONSTRAINT accounts_profile_version CHECK (profile_version >= 0)
);
COMMENT ON TABLE dm.accounts IS
    'Silicon Accounts accounts seen by DM: uuid (storage key), kind, current public id for display, custodian of a Silicon, and the sign-in cutoff set by webhooks.';
COMMENT ON COLUMN dm.accounts.revoked_before IS
    'Access tokens issued before this instant are refused (sign-out with a reason other than app_revoked, access removed, deleted).';
CREATE INDEX accounts_current_id_idx ON dm.accounts (current_id) WHERE current_id IS NOT NULL;
CREATE INDEX accounts_custodian_idx ON dm.accounts (custodian_uuid) WHERE custodian_uuid IS NOT NULL;

-- IAM-era identities linked to Accounts uuids by `dm-migrate link-identities`.
CREATE TABLE dm.identity_links (
    iam_public_id text COLLATE "C" PRIMARY KEY,
    iam_principal_id uuid,
    kind dm.actor_kind NOT NULL,
    accounts_uuid text COLLATE "C" NOT NULL,
    previous_uuid text COLLATE "C",
    source text NOT NULL,
    run_id uuid NOT NULL,
    linked_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT identity_links_public_id_format
        CHECK (iam_public_id ~ '^(c|si):[a-z0-9_-]{3,50}$'),
    CONSTRAINT identity_links_kind_matches
        CHECK ((kind = 'carbon') = (iam_public_id LIKE 'c:%')),
    CONSTRAINT identity_links_uuid_format CHECK (accounts_uuid ~ '^[A-Za-z0-9]{3,64}$'),
    CONSTRAINT identity_links_source_length CHECK (char_length(source) BETWEEN 1 AND 1024),
    CONSTRAINT identity_links_one_identity_per_account UNIQUE (accounts_uuid)
);
COMMENT ON TABLE dm.identity_links IS
    'Every IAM public id re-keyed to an Accounts uuid, so the re-key is auditable and can be re-run with a corrected mapping.';

CREATE TABLE dm.identity_link_runs (
    run_id uuid PRIMARY KEY,
    source text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    report jsonb NOT NULL CHECK (jsonb_typeof(report) = 'object')
);

-- Signed Accounts webhook deliveries already applied (dedupe on event_id).
CREATE TABLE dm.accounts_webhook_receipts (
    event_id text PRIMARY KEY,
    event_type text NOT NULL,
    account_uuid text COLLATE "C",
    occurred_at timestamptz,
    received_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    outcome text NOT NULL,
    CONSTRAINT accounts_webhook_receipts_event_id_length
        CHECK (char_length(event_id) BETWEEN 1 AND 255),
    CONSTRAINT accounts_webhook_receipts_event_type_length
        CHECK (char_length(event_type) BETWEEN 1 AND 255),
    CONSTRAINT accounts_webhook_receipts_outcome
        CHECK (outcome IN ('applied', 'stale', 'ignored'))
);
CREATE INDEX accounts_webhook_receipts_received_idx ON dm.accounts_webhook_receipts (received_at);

-- Groups: co-equal owners and invitations that must be accepted.
CREATE TABLE dm.group_owners (
    conversation_id uuid NOT NULL REFERENCES dm.groups (conversation_id),
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    added_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    added_by_id text,
    PRIMARY KEY (conversation_id, actor_kind, actor_id)
);
COMMENT ON TABLE dm.group_owners IS
    'Co-equal group owners (the creator first). A group always keeps at least one owner.';
CREATE INDEX group_owners_actor_idx ON dm.group_owners (actor_kind, actor_id);

-- The creator of every existing group becomes its first owner.
INSERT INTO dm.group_owners (conversation_id, actor_kind, actor_id, added_at)
SELECT g.conversation_id, c.created_by_kind, c.created_by_id, c.created_at
FROM dm.groups g
JOIN dm.conversations c ON c.id = g.conversation_id
ON CONFLICT DO NOTHING;

-- Existing invitation rows were immediate memberships, so they stay accepted.
ALTER TABLE dm.group_invitations
    ADD COLUMN status text NOT NULL DEFAULT 'accepted',
    ADD COLUMN invited_by_kind dm.actor_kind,
    ADD COLUMN invited_by_id text,
    ADD COLUMN invited_at timestamptz,
    ADD COLUMN accepted_at timestamptz,
    ADD CONSTRAINT group_invitations_status CHECK (status IN ('pending', 'accepted')),
    ADD CONSTRAINT group_invitations_inviter
        CHECK ((invited_by_kind IS NULL) = (invited_by_id IS NULL));
CREATE INDEX group_invitations_actor_idx
    ON dm.group_invitations (actor_kind, actor_id, status);

-- Who may start conversations with, or invite, a Silicon outside its custodian's circle.
CREATE TABLE dm.silicon_contact_allowances (
    silicon_id text COLLATE "C" NOT NULL,
    allowed_kind dm.actor_kind NOT NULL,
    allowed_id text COLLATE "C" NOT NULL,
    added_by_id text COLLATE "C" NOT NULL,
    added_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (silicon_id, allowed_kind, allowed_id),
    CONSTRAINT silicon_contact_allowances_not_self CHECK (silicon_id <> allowed_id)
);
COMMENT ON TABLE dm.silicon_contact_allowances IS
    'A Silicon''s allow-list, managed by the Silicon or its custodian. Accounts are uuids.';
CREATE INDEX silicon_contact_allowances_allowed_idx
    ON dm.silicon_contact_allowances (allowed_kind, allowed_id);

-- One canonical direct conversation per exact participant set. A pair that had
-- conversations in two organizations keeps both histories; the less recently active
-- one will point at the canonical one (marked, and the uniqueness enforced, in 0040).
ALTER TABLE dm.conversations
    ADD COLUMN superseded_by uuid,
    ADD CONSTRAINT conversations_superseded_by_fk
        FOREIGN KEY (superseded_by) REFERENCES dm.conversations (id),
    ADD CONSTRAINT conversations_not_self_superseded
        CHECK (superseded_by IS NULL OR superseded_by <> id);

-- The app that sent a message for an account through a User verification proof.
ALTER TABLE dm.messages
    ADD COLUMN via_app text,
    ADD CONSTRAINT messages_via_app_format
        CHECK (via_app IS NULL OR via_app ~ '^[a-z][a-z0-9_-]{0,79}$');

-- Each account reads one delivery stream (in the @accounts partition); sync starts above
-- the sequences that existed when the stream was created.
ALTER TABLE dm.actor_delivery_streams
    ADD COLUMN sync_floor bigint NOT NULL DEFAULT 0,
    ADD CONSTRAINT actor_delivery_streams_sync_floor
        CHECK (sync_floor >= 0 AND sync_floor < next_sequence);

-- Ting handoffs are never deleted; while Ting is not configured they are marked
-- discarded with a reason instead of being sent.
ALTER TABLE dm.ting_handoffs
    ADD COLUMN discarded_at timestamptz,
    ADD COLUMN discard_reason text,
    ADD CONSTRAINT ting_handoffs_discard_consistency
        CHECK ((discarded_at IS NULL) = (discard_reason IS NULL)),
    ADD CONSTRAINT ting_handoffs_discard_reason_length
        CHECK (discard_reason IS NULL OR char_length(discard_reason) BETWEEN 1 AND 255),
    ADD CONSTRAINT ting_handoffs_discarded_not_accepted
        CHECK (discarded_at IS NULL OR accepted_at IS NULL);
CREATE INDEX ting_handoffs_pending_idx
    ON dm.ting_handoffs (next_attempt_at, created_at, delivery_id)
    WHERE accepted_at IS NULL AND discarded_at IS NULL;

-- Encrypted proof refresh tokens (`sapr_…`) for Ting enrolment, keyed by DM_DATA_KEY.
CREATE TABLE dm.ting_proof_grants (
    app_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text COLLATE "C" NOT NULL,
    purpose text NOT NULL,
    proof_id text NOT NULL,
    nonce bytea NOT NULL,
    ciphertext bytea NOT NULL,
    refresh_expires_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (app_id, actor_kind, actor_id, purpose),
    CONSTRAINT ting_proof_grants_purpose CHECK (purpose IN ('tings.subscribe')),
    CONSTRAINT ting_proof_grants_proof_id_length CHECK (char_length(proof_id) BETWEEN 1 AND 255),
    CONSTRAINT ting_proof_grants_nonce_length CHECK (octet_length(nonce) = 12),
    CONSTRAINT ting_proof_grants_ciphertext_length
        CHECK (octet_length(ciphertext) BETWEEN 17 AND 65552)
);

-- Organization-free access paths.
CREATE INDEX conversation_participants_actor_idx
    ON dm.conversation_participants (actor_kind, actor_id, conversation_id);
CREATE INDEX conversations_creator_recent_idx
    ON dm.conversations (created_by_kind, created_by_id, created_at DESC);
CREATE INDEX actor_deliveries_target_sequence_idx
    ON dm.actor_deliveries (target_kind, target_id, sequence);
CREATE INDEX recent_gifs_carbon_recency_idx
    ON dm.recent_gifs (carbon_id, last_used_at DESC, provider_id DESC);
CREATE INDEX drafts_owner_idx ON dm.drafts (actor_kind, actor_id);
CREATE INDEX client_presence_leases_actor_idx
    ON dm.client_presence_leases (actor_kind, actor_id);
