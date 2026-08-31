-- Silicon DM tenant directory snapshots and conversation aggregates.
--
-- IAM remains authoritative. These rows are intentionally minimal snapshots used to
-- enforce tenant-safe foreign keys and to retain the identity attached to durable DM data.

CREATE SCHEMA IF NOT EXISTS dm;
CREATE SCHEMA IF NOT EXISTS dm_private;

CREATE TYPE dm.actor_kind AS ENUM ('carbon', 'silicon');
CREATE TYPE dm.snapshot_status AS ENUM ('active', 'suspended', 'deleted');

CREATE FUNCTION dm_private.set_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.updated_at := GREATEST(OLD.updated_at, clock_timestamp());
    RETURN NEW;
END;
$$;

COMMENT ON FUNCTION dm_private.set_updated_at() IS
    'Advances updated_at with the database wall clock without moving backward after lock waits.';

CREATE TABLE dm.organization_snapshots (
    organization_id text PRIMARY KEY,
    status dm.snapshot_status NOT NULL DEFAULT 'active',
    iam_version bigint NOT NULL,
    refreshed_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    CONSTRAINT organization_snapshots_id_length
        CHECK (char_length(organization_id) BETWEEN 1 AND 255),
    CONSTRAINT organization_snapshots_id_no_control_characters
        CHECK (organization_id !~ '[[:cntrl:]]'),
    CONSTRAINT organization_snapshots_positive_iam_version CHECK (iam_version > 0),
    CONSTRAINT organization_snapshots_refresh_order CHECK (refreshed_at >= created_at)
);

COMMENT ON TABLE dm.organization_snapshots IS
    'Minimal IAM organization state cached only for tenancy integrity; IAM is authoritative.';

CREATE TRIGGER organization_snapshots_set_updated_at
BEFORE UPDATE ON dm.organization_snapshots
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE FUNCTION dm_private.enforce_organization_snapshot_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'organization snapshot identity is immutable' USING ERRCODE = '23514';
    END IF;
    IF NEW.iam_version < OLD.iam_version OR NEW.refreshed_at < OLD.refreshed_at THEN
        RAISE EXCEPTION 'organization snapshot version and refresh time cannot move backward'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER organization_snapshots_enforce_update_policy
BEFORE UPDATE ON dm.organization_snapshots
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_organization_snapshot_update_policy();

CREATE INDEX organization_snapshots_status_idx
    ON dm.organization_snapshots (status, refreshed_at);

CREATE TABLE dm.actor_snapshots (
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    status dm.snapshot_status NOT NULL DEFAULT 'active',
    iam_version bigint NOT NULL,
    refreshed_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, actor_kind, actor_id),
    CONSTRAINT actor_snapshots_organization_fk
        FOREIGN KEY (organization_id)
        REFERENCES dm.organization_snapshots (organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT actor_snapshots_id_length
        CHECK (char_length(actor_id) BETWEEN 1 AND 255),
    CONSTRAINT actor_snapshots_id_no_control_characters
        CHECK (actor_id !~ '[[:cntrl:]]'),
    CONSTRAINT actor_snapshots_positive_iam_version CHECK (iam_version > 0),
    CONSTRAINT actor_snapshots_refresh_order CHECK (refreshed_at >= created_at)
);

COMMENT ON TABLE dm.actor_snapshots IS
    'Minimal per-organization IAM actor snapshot used for durable ownership and authorization joins.';

CREATE TRIGGER actor_snapshots_set_updated_at
BEFORE UPDATE ON dm.actor_snapshots
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE FUNCTION dm_private.enforce_actor_snapshot_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.actor_kind IS DISTINCT FROM OLD.actor_kind
        OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'actor snapshot identity is immutable' USING ERRCODE = '23514';
    END IF;
    IF NEW.iam_version < OLD.iam_version OR NEW.refreshed_at < OLD.refreshed_at THEN
        RAISE EXCEPTION 'actor snapshot version and refresh time cannot move backward'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER actor_snapshots_enforce_update_policy
BEFORE UPDATE ON dm.actor_snapshots
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_actor_snapshot_update_policy();

CREATE INDEX actor_snapshots_actor_lookup_idx
    ON dm.actor_snapshots (actor_kind, actor_id, organization_id);
CREATE INDEX actor_snapshots_refresh_idx
    ON dm.actor_snapshots (status, refreshed_at);

CREATE TABLE dm.conversations (
    id uuid PRIMARY KEY,
    organization_id text NOT NULL,
    participant_set_hash bytea NOT NULL,
    participant_set jsonb,
    participant_set_fingerprint bytea,
    participants_sealed_at timestamptz,
    next_message_sequence bigint NOT NULL DEFAULT 1,
    created_by_kind dm.actor_kind NOT NULL,
    created_by_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (id, organization_id),
    CONSTRAINT conversations_organization_fk
        FOREIGN KEY (organization_id)
        REFERENCES dm.organization_snapshots (organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT conversations_creator_fk
        FOREIGN KEY (organization_id, created_by_kind, created_by_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT,
    CONSTRAINT conversations_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT conversations_participant_set_hash_length
        CHECK (octet_length(participant_set_hash) = 32),
    CONSTRAINT conversations_participant_set_shape CHECK (
        participant_set IS NULL OR jsonb_typeof(participant_set) = 'array'
    ),
    CONSTRAINT conversations_participant_set_fingerprint_length CHECK (
        participant_set_fingerprint IS NULL
        OR octet_length(participant_set_fingerprint) = 32
    ),
    CONSTRAINT conversations_participant_seal_consistency CHECK (
        (participant_set IS NULL)
            = (participant_set_fingerprint IS NULL)
        AND (participant_set IS NULL)
            = (participants_sealed_at IS NULL)
    ),
    CONSTRAINT conversations_participant_seal_order CHECK (
        participants_sealed_at IS NULL OR participants_sealed_at >= created_at
    ),
    CONSTRAINT conversations_positive_next_message_sequence
        CHECK (next_message_sequence > 0),
    CONSTRAINT conversations_exact_participant_set
        UNIQUE (organization_id, participant_set_hash),
    CONSTRAINT conversations_database_exact_participant_set
        UNIQUE (organization_id, participant_set_fingerprint)
);

COMMENT ON TABLE dm.conversations IS
    'One immutable exact participant set per organization with a row-locked message sequence allocator.';
COMMENT ON COLUMN dm.conversations.participant_set_hash IS
    'BLAKE3-256 of the application canonicalized, length-delimited, sorted actor-kind/actor-id set.';
COMMENT ON COLUMN dm.conversations.participant_set IS
    'Database-canonical immutable actor set, retained to independently seal and audit membership.';
COMMENT ON COLUMN dm.conversations.participant_set_fingerprint IS
    'Database-computed SHA-256 of participant_set, preventing duplicate exact sets even if an application hash is incorrect.';
COMMENT ON COLUMN dm.conversations.next_message_sequence IS
    'Next sequence allocated transactionally by the message insertion trigger.';

CREATE TRIGGER conversations_set_updated_at
BEFORE UPDATE ON dm.conversations
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE FUNCTION dm_private.enforce_conversation_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.participant_set_hash IS DISTINCT FROM OLD.participant_set_hash
        OR NEW.created_by_kind IS DISTINCT FROM OLD.created_by_kind
        OR NEW.created_by_id IS DISTINCT FROM OLD.created_by_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'conversation identity and exact participant set are immutable'
            USING ERRCODE = '23514';
    END IF;

    IF OLD.participants_sealed_at IS NOT NULL AND (
        NEW.participant_set IS DISTINCT FROM OLD.participant_set
        OR NEW.participant_set_fingerprint IS DISTINCT FROM OLD.participant_set_fingerprint
        OR NEW.participants_sealed_at IS DISTINCT FROM OLD.participants_sealed_at
    ) THEN
        RAISE EXCEPTION 'sealed conversation participant metadata is immutable'
            USING ERRCODE = '23514';
    END IF;

    IF OLD.participants_sealed_at IS NULL AND NEW.participants_sealed_at IS NOT NULL AND (
        NEW.participant_set IS NULL OR NEW.participant_set_fingerprint IS NULL
    ) THEN
        RAISE EXCEPTION 'conversation participant metadata must be sealed atomically'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.next_message_sequence NOT IN (
        OLD.next_message_sequence,
        OLD.next_message_sequence + 1
    ) THEN
        RAISE EXCEPTION 'conversation message sequence must advance by exactly one'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER conversations_enforce_update_policy
BEFORE UPDATE ON dm.conversations
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_conversation_update_policy();

CREATE INDEX conversations_organization_updated_idx
    ON dm.conversations (organization_id, updated_at DESC, id DESC);
CREATE INDEX conversations_creator_idx
    ON dm.conversations (organization_id, created_by_kind, created_by_id, created_at DESC);

CREATE TABLE dm.conversation_participants (
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    added_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (conversation_id, actor_kind, actor_id),
    UNIQUE (conversation_id, organization_id, actor_kind, actor_id),
    CONSTRAINT conversation_participants_conversation_fk
        FOREIGN KEY (conversation_id, organization_id)
        REFERENCES dm.conversations (id, organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT conversation_participants_actor_fk
        FOREIGN KEY (organization_id, actor_kind, actor_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT
);

COMMENT ON TABLE dm.conversation_participants IS
    'Immutable membership snapshot for the exact participant set represented by a conversation.';

CREATE FUNCTION dm_private.enforce_conversation_participant_immutability()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'conversation participants are immutable' USING ERRCODE = '23514';
    END IF;

    IF NEW.conversation_id IS DISTINCT FROM OLD.conversation_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.actor_kind IS DISTINCT FROM OLD.actor_kind
        OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
        OR NEW.added_at IS DISTINCT FROM OLD.added_at
    THEN
        RAISE EXCEPTION 'conversation participants are immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION dm_private.guard_conversation_participant_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    sealed_at timestamptz;
BEGIN
    SELECT participants_sealed_at
    INTO sealed_at
    FROM dm.conversations
    WHERE id = NEW.conversation_id
      AND organization_id = NEW.organization_id
    FOR UPDATE;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'conversation % does not exist in organization %',
            NEW.conversation_id,
            NEW.organization_id
            USING ERRCODE = '23503';
    END IF;

    IF sealed_at IS NOT NULL THEN
        RAISE EXCEPTION 'conversation participants are sealed and immutable'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER conversation_participants_guard_insert
BEFORE INSERT ON dm.conversation_participants
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_conversation_participant_insert();

CREATE TRIGGER conversation_participants_enforce_immutability
BEFORE UPDATE OR DELETE ON dm.conversation_participants
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_conversation_participant_immutability();

CREATE INDEX conversation_participants_actor_conversations_idx
    ON dm.conversation_participants (
        organization_id,
        actor_kind,
        actor_id,
        conversation_id
    );

CREATE FUNCTION dm_private.assert_conversation_participant_count(p_conversation_id uuid)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    participant_count bigint;
    canonical_participant_set jsonb;
    canonical_fingerprint bytea;
    stored_participant_set jsonb;
    stored_fingerprint bytea;
BEGIN
    SELECT participant_set, participant_set_fingerprint
    INTO stored_participant_set, stored_fingerprint
    FROM dm.conversations
    WHERE id = p_conversation_id
    FOR UPDATE;

    IF NOT FOUND THEN
        RETURN;
    END IF;

    SELECT
        count(*),
        COALESCE(
            jsonb_agg(
                jsonb_build_object(
                    'type', participant.actor_kind::text,
                    'id', participant.actor_id
                )
                ORDER BY participant.actor_kind::text, participant.actor_id
            ),
            '[]'::jsonb
        )
    INTO participant_count, canonical_participant_set
    FROM dm.conversation_participants
        AS participant
    WHERE conversation_id = p_conversation_id;

    IF participant_count < 2 THEN
        RAISE EXCEPTION 'conversation % must have at least two participants', p_conversation_id
            USING ERRCODE = '23514';
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM dm.conversations AS conversation
        JOIN dm.conversation_participants AS participant
          ON participant.conversation_id = conversation.id
         AND participant.organization_id = conversation.organization_id
         AND participant.actor_kind = conversation.created_by_kind
         AND participant.actor_id = conversation.created_by_id
        WHERE conversation.id = p_conversation_id
    ) THEN
        RAISE EXCEPTION 'conversation % creator must be a participant', p_conversation_id
            USING ERRCODE = '23514';
    END IF;

    canonical_fingerprint := sha256(
        convert_to(canonical_participant_set::text, 'UTF8')
    );

    IF stored_participant_set IS NOT NULL AND (
        stored_participant_set IS DISTINCT FROM canonical_participant_set
        OR stored_fingerprint IS DISTINCT FROM canonical_fingerprint
    ) THEN
        RAISE EXCEPTION 'conversation % participant set no longer matches its seal',
            p_conversation_id
            USING ERRCODE = '23514';
    END IF;

    UPDATE dm.conversations
    SET participant_set = canonical_participant_set,
        participant_set_fingerprint = canonical_fingerprint,
        participants_sealed_at = transaction_timestamp()
    WHERE id = p_conversation_id
      AND participants_sealed_at IS NULL;
END;
$$;

CREATE FUNCTION dm_private.check_conversation_participant_count_from_conversation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM dm_private.assert_conversation_participant_count(NEW.id);
    RETURN NEW;
END;
$$;

CREATE FUNCTION dm_private.check_conversation_participant_count_from_participant()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_conversation_id uuid;
BEGIN
    IF TG_OP = 'DELETE' THEN
        target_conversation_id := OLD.conversation_id;
    ELSE
        target_conversation_id := NEW.conversation_id;
    END IF;

    PERFORM dm_private.assert_conversation_participant_count(target_conversation_id);

    IF TG_OP = 'UPDATE' AND NEW.conversation_id IS DISTINCT FROM OLD.conversation_id THEN
        PERFORM dm_private.assert_conversation_participant_count(OLD.conversation_id);
    END IF;

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER conversations_require_participants
AFTER INSERT ON dm.conversations
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION
    dm_private.check_conversation_participant_count_from_conversation();

CREATE CONSTRAINT TRIGGER conversation_participants_enforce_minimum
AFTER INSERT OR UPDATE OR DELETE ON dm.conversation_participants
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION
    dm_private.check_conversation_participant_count_from_participant();
