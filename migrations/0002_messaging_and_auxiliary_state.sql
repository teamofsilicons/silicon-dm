-- Durable messages, receipts, bundles, drafts, GIF history, and Hook events.

CREATE TYPE dm.message_status AS ENUM ('waiting', 'sent', 'delivered', 'read', 'failed');
CREATE TYPE dm.attachment_kind AS ENUM ('attachment', 'voice');
CREATE TYPE dm.transcription_result AS ENUM ('not_applicable', 'succeeded', 'failed');
CREATE TYPE dm.receipt_status AS ENUM ('delivered', 'read');
CREATE TYPE dm.bundle_role AS ENUM ('display', 'member');

CREATE TABLE dm.messages (
    id uuid PRIMARY KEY,
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    sender_kind dm.actor_kind NOT NULL,
    sender_id text NOT NULL,
    sequence bigint NOT NULL,
    status dm.message_status NOT NULL DEFAULT 'sent',
    text_content text,
    voice_transcript text,
    transcription_result dm.transcription_result NOT NULL DEFAULT 'not_applicable',
    content_hash bytea NOT NULL,
    failure_reason text,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    content_sealed_at timestamptz,
    outbox_enqueued_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    delivered_at timestamptz,
    read_at timestamptz,
    UNIQUE (id, organization_id),
    UNIQUE (id, conversation_id, organization_id),
    CONSTRAINT messages_conversation_fk
        FOREIGN KEY (conversation_id, organization_id)
        REFERENCES dm.conversations (id, organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT messages_sender_participant_fk
        FOREIGN KEY (conversation_id, organization_id, sender_kind, sender_id)
        REFERENCES dm.conversation_participants (
            conversation_id,
            organization_id,
            actor_kind,
            actor_id
        )
        ON DELETE RESTRICT,
    CONSTRAINT messages_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT messages_positive_sequence CHECK (sequence > 0),
    CONSTRAINT messages_conversation_sequence UNIQUE (conversation_id, sequence),
    CONSTRAINT messages_text_length
        CHECK (text_content IS NULL OR char_length(text_content) <= 100000000),
    CONSTRAINT messages_transcript_length
        CHECK (voice_transcript IS NULL OR char_length(voice_transcript) <= 100000000),
    CONSTRAINT messages_content_hash_length CHECK (octet_length(content_hash) = 32),
    CONSTRAINT messages_content_seal_order CHECK (
        content_sealed_at IS NULL OR content_sealed_at >= created_at
    ),
    CONSTRAINT messages_outbox_enqueue_order CHECK (outbox_enqueued_at >= created_at),
    CONSTRAINT messages_failure_reason_length
        CHECK (failure_reason IS NULL OR char_length(failure_reason) BETWEEN 1 AND 2000),
    CONSTRAINT messages_failure_consistency CHECK (
        (status = 'failed' AND failure_reason IS NOT NULL)
        OR (status <> 'failed' AND failure_reason IS NULL)
    ),
    CONSTRAINT messages_delivery_timestamp_consistency CHECK (
        (status IN ('waiting', 'sent', 'failed') AND delivered_at IS NULL AND read_at IS NULL)
        OR (status = 'delivered' AND delivered_at IS NOT NULL AND read_at IS NULL)
        OR (
            status = 'read'
            AND delivered_at IS NOT NULL
            AND read_at IS NOT NULL
            AND read_at >= delivered_at
        )
    ),
    CONSTRAINT messages_delivery_after_creation
        CHECK (delivered_at IS NULL OR delivered_at >= created_at),
    CONSTRAINT messages_read_after_creation CHECK (read_at IS NULL OR read_at >= created_at)
);

COMMENT ON TABLE dm.messages IS
    'Immutable message content plus monotonic aggregate delivery state in conversation sequence order.';
COMMENT ON COLUMN dm.messages.sequence IS
    'Assigned by PostgreSQL while locking the owning conversation row; clients never choose it.';
COMMENT ON COLUMN dm.messages.content_hash IS
    'BLAKE3-256 of canonical user-visible content, used for safe draft clearing and idempotency.';
COMMENT ON COLUMN dm.messages.outbox_enqueued_at IS
    'Transaction time at which the source was created; a deferred constraint proves its recipient outbox was complete before commit.';

CREATE FUNCTION dm_private.assign_message_sequence()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    allocated_sequence bigint;
BEGIN
    UPDATE dm.conversations
    SET next_message_sequence = next_message_sequence + 1
    WHERE id = NEW.conversation_id
      AND organization_id = NEW.organization_id
    RETURNING next_message_sequence - 1 INTO allocated_sequence;

    IF allocated_sequence IS NULL THEN
        RAISE EXCEPTION 'conversation % does not exist in organization %',
            NEW.conversation_id,
            NEW.organization_id
            USING ERRCODE = '23503';
    END IF;

    NEW.sequence := allocated_sequence;
    RETURN NEW;
END;
$$;

CREATE FUNCTION dm_private.enforce_message_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.conversation_id IS DISTINCT FROM OLD.conversation_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.sender_kind IS DISTINCT FROM OLD.sender_kind
        OR NEW.sender_id IS DISTINCT FROM OLD.sender_id
        OR NEW.sequence IS DISTINCT FROM OLD.sequence
        OR NEW.text_content IS DISTINCT FROM OLD.text_content
        OR NEW.voice_transcript IS DISTINCT FROM OLD.voice_transcript
        OR NEW.transcription_result IS DISTINCT FROM OLD.transcription_result
        OR NEW.content_hash IS DISTINCT FROM OLD.content_hash
        OR NEW.outbox_enqueued_at IS DISTINCT FROM OLD.outbox_enqueued_at
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'message identity and content are immutable' USING ERRCODE = '23514';
    END IF;

    IF OLD.content_sealed_at IS NOT NULL
        AND NEW.content_sealed_at IS DISTINCT FROM OLD.content_sealed_at
    THEN
        RAISE EXCEPTION 'message content seal is immutable once set'
            USING ERRCODE = '23514';
    END IF;

    IF NOT (
        NEW.status = OLD.status
        OR (OLD.status = 'waiting' AND NEW.status IN ('sent', 'delivered', 'read', 'failed'))
        OR (OLD.status = 'sent' AND NEW.status IN ('delivered', 'read', 'failed'))
        OR (OLD.status = 'delivered' AND NEW.status = 'read')
    ) THEN
        RAISE EXCEPTION 'message status cannot move from % to %', OLD.status, NEW.status
            USING ERRCODE = '23514';
    END IF;

    IF OLD.delivered_at IS NOT NULL AND NEW.delivered_at IS DISTINCT FROM OLD.delivered_at THEN
        RAISE EXCEPTION 'message delivery timestamp is immutable once set'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.read_at IS NOT NULL AND NEW.read_at IS DISTINCT FROM OLD.read_at THEN
        RAISE EXCEPTION 'message read timestamp is immutable once set'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.status = 'failed' AND NEW.failure_reason IS DISTINCT FROM OLD.failure_reason THEN
        RAISE EXCEPTION 'message failure reason is immutable once failed'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER messages_assign_sequence
BEFORE INSERT ON dm.messages
FOR EACH ROW EXECUTE FUNCTION dm_private.assign_message_sequence();

CREATE TRIGGER messages_enforce_update_policy
BEFORE UPDATE ON dm.messages
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_message_update_policy();

CREATE TRIGGER messages_set_updated_at
BEFORE UPDATE ON dm.messages
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE INDEX messages_conversation_page_idx
    ON dm.messages (conversation_id, sequence DESC);
CREATE INDEX messages_sender_idx
    ON dm.messages (organization_id, sender_kind, sender_id, created_at DESC, id DESC);
CREATE INDEX messages_status_idx
    ON dm.messages (status, updated_at, id)
    WHERE status IN ('waiting', 'sent', 'delivered');

CREATE TABLE dm.message_attachments (
    message_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    position smallint NOT NULL,
    attachment_kind dm.attachment_kind NOT NULL DEFAULT 'attachment',
    permanent_url text NOT NULL,
    name text,
    content_type text,
    declared_size_bytes bigint,
    duration_milliseconds bigint,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (message_id, position),
    CONSTRAINT message_attachments_message_fk
        FOREIGN KEY (message_id, conversation_id, organization_id)
        REFERENCES dm.messages (id, conversation_id, organization_id)
        ON DELETE CASCADE,
    CONSTRAINT message_attachments_position CHECK (
        (attachment_kind = 'attachment' AND position BETWEEN 0 AND 99)
        OR (attachment_kind = 'voice' AND position = 100)
    ),
    CONSTRAINT message_attachments_https_url CHECK (
        char_length(permanent_url) BETWEEN 9 AND 8192
        AND permanent_url ~ '^https://[^[:space:]]+$'
    ),
    CONSTRAINT message_attachments_name_length
        CHECK (name IS NULL OR char_length(name) BETWEEN 1 AND 1024),
    CONSTRAINT message_attachments_content_type_length
        CHECK (content_type IS NULL OR char_length(content_type) BETWEEN 1 AND 255),
    CONSTRAINT message_attachments_size_limit CHECK (
        declared_size_bytes IS NULL
        OR declared_size_bytes BETWEEN 0 AND 5368709120
    ),
    CONSTRAINT message_attachments_duration_limit CHECK (
        duration_milliseconds IS NULL
        OR duration_milliseconds BETWEEN 0 AND 172800000
    ),
    CONSTRAINT message_attachments_duration_kind CHECK (
        attachment_kind = 'voice' OR duration_milliseconds IS NULL
    )
);

CREATE UNIQUE INDEX message_attachments_one_voice_idx
    ON dm.message_attachments (message_id)
    WHERE attachment_kind = 'voice';

CREATE TABLE dm.message_gifs (
    message_id uuid PRIMARY KEY,
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    provider_id text NOT NULL,
    url text NOT NULL,
    preview_url text,
    title text,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    CONSTRAINT message_gifs_message_fk
        FOREIGN KEY (message_id, conversation_id, organization_id)
        REFERENCES dm.messages (id, conversation_id, organization_id)
        ON DELETE CASCADE,
    CONSTRAINT message_gifs_provider_id_length
        CHECK (char_length(provider_id) BETWEEN 1 AND 255),
    CONSTRAINT message_gifs_url CHECK (
        char_length(url) BETWEEN 9 AND 8192
        AND url ~ '^https://[^[:space:]]+$'
    ),
    CONSTRAINT message_gifs_preview_url CHECK (
        preview_url IS NULL
        OR (
            char_length(preview_url) BETWEEN 9 AND 8192
            AND preview_url ~ '^https://[^[:space:]]+$'
        )
    ),
    CONSTRAINT message_gifs_title_length
        CHECK (title IS NULL OR char_length(title) <= 1000)
);

CREATE FUNCTION dm_private.guard_message_content_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_message_id uuid;
    sealed_at timestamptz;
BEGIN
    IF TG_OP = 'DELETE' THEN
        target_message_id := OLD.message_id;
    ELSE
        target_message_id := NEW.message_id;
    END IF;

    SELECT content_sealed_at
    INTO sealed_at
    FROM dm.messages
    WHERE id = target_message_id
    FOR UPDATE;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'message % does not exist', target_message_id
            USING ERRCODE = '23503';
    END IF;

    IF sealed_at IS NOT NULL THEN
        RAISE EXCEPTION 'message content is sealed and immutable'
            USING ERRCODE = '23514';
    END IF;

    IF TG_OP = 'UPDATE' AND NEW.message_id IS DISTINCT FROM OLD.message_id THEN
        RAISE EXCEPTION 'message content rows cannot move between messages'
            USING ERRCODE = '23514';
    END IF;

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER message_attachments_guard_content_mutation
BEFORE INSERT OR UPDATE OR DELETE ON dm.message_attachments
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_message_content_mutation();

CREATE TRIGGER message_gifs_guard_content_mutation
BEFORE INSERT OR UPDATE OR DELETE ON dm.message_gifs
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_message_content_mutation();

CREATE FUNCTION dm_private.assert_message_content(p_message_id uuid)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    message_row dm.messages%ROWTYPE;
    has_attachment boolean;
    has_voice boolean;
    has_gif boolean;
    attachment_count bigint;
BEGIN
    SELECT *
    INTO message_row
    FROM dm.messages
    WHERE id = p_message_id
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    SELECT
        count(*) > 0,
        count(*) FILTER (WHERE attachment_kind = 'voice') > 0,
        count(*)
    INTO has_attachment, has_voice, attachment_count
    FROM dm.message_attachments
    WHERE message_id = p_message_id;

    SELECT EXISTS (SELECT 1 FROM dm.message_gifs WHERE message_id = p_message_id)
    INTO has_gif;

    IF attachment_count > 100 THEN
        RAISE EXCEPTION 'message % cannot contain more than 100 combined attachments and voice items',
            p_message_id
            USING ERRCODE = '23514';
    END IF;

    IF message_row.text_content IS NULL AND NOT has_attachment AND NOT has_gif THEN
        RAISE EXCEPTION 'message % must contain text, an attachment, voice, or a GIF', p_message_id
            USING ERRCODE = '23514';
    END IF;

    IF has_voice THEN
        IF message_row.transcription_result = 'not_applicable' THEN
            RAISE EXCEPTION 'voice message % must record a transcription outcome', p_message_id
                USING ERRCODE = '23514';
        END IF;
        IF message_row.transcription_result = 'succeeded'
            AND message_row.voice_transcript IS NULL
        THEN
            RAISE EXCEPTION 'successful transcription for message % requires transcript text',
                p_message_id
                USING ERRCODE = '23514';
        END IF;
        IF message_row.transcription_result = 'failed'
            AND message_row.voice_transcript IS NOT NULL
        THEN
            RAISE EXCEPTION 'failed transcription for message % cannot contain transcript text',
                p_message_id
                USING ERRCODE = '23514';
        END IF;
    ELSIF message_row.transcription_result <> 'not_applicable'
        OR message_row.voice_transcript IS NOT NULL
    THEN
        RAISE EXCEPTION 'message % has transcription data without a voice attachment', p_message_id
            USING ERRCODE = '23514';
    END IF;

    UPDATE dm.messages
    SET content_sealed_at = transaction_timestamp()
    WHERE id = p_message_id
      AND content_sealed_at IS NULL;
END;
$$;

CREATE FUNCTION dm_private.check_message_content_from_message()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM dm_private.assert_message_content(NEW.id);
    RETURN NEW;
END;
$$;

CREATE FUNCTION dm_private.check_message_content_from_child()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM dm_private.assert_message_content(OLD.message_id);
        RETURN OLD;
    END IF;

    PERFORM dm_private.assert_message_content(NEW.message_id);
    IF TG_OP = 'UPDATE' AND NEW.message_id IS DISTINCT FROM OLD.message_id THEN
        PERFORM dm_private.assert_message_content(OLD.message_id);
    END IF;
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER messages_require_content
AFTER INSERT OR UPDATE ON dm.messages
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_message_content_from_message();

CREATE CONSTRAINT TRIGGER message_attachments_validate_content
AFTER INSERT OR UPDATE OR DELETE ON dm.message_attachments
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_message_content_from_child();

CREATE CONSTRAINT TRIGGER message_gifs_validate_content
AFTER INSERT OR UPDATE OR DELETE ON dm.message_gifs
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_message_content_from_child();

CREATE TABLE dm.message_receipts (
    id uuid PRIMARY KEY,
    message_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    recipient_kind dm.actor_kind NOT NULL,
    recipient_id text NOT NULL,
    device_id text NOT NULL,
    status dm.receipt_status NOT NULL,
    delivered_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    read_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (id, conversation_id, organization_id),
    UNIQUE (id, message_id, conversation_id, organization_id),
    CONSTRAINT message_receipts_message_fk
        FOREIGN KEY (message_id, conversation_id, organization_id)
        REFERENCES dm.messages (id, conversation_id, organization_id)
        ON DELETE CASCADE,
    CONSTRAINT message_receipts_recipient_participant_fk
        FOREIGN KEY (conversation_id, organization_id, recipient_kind, recipient_id)
        REFERENCES dm.conversation_participants (
            conversation_id,
            organization_id,
            actor_kind,
            actor_id
        )
        ON DELETE RESTRICT,
    CONSTRAINT message_receipts_natural_key
        UNIQUE (message_id, recipient_kind, recipient_id, device_id),
    CONSTRAINT message_receipts_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT message_receipts_device_id_length
        CHECK (char_length(device_id) BETWEEN 1 AND 255),
    CONSTRAINT message_receipts_device_id_no_control_characters
        CHECK (device_id !~ '[[:cntrl:]]'),
    CONSTRAINT message_receipts_read_consistency CHECK (
        (status = 'delivered' AND read_at IS NULL)
        OR (status = 'read' AND read_at IS NOT NULL AND read_at >= delivered_at)
    ),
    CONSTRAINT message_receipts_delivery_after_creation
        CHECK (delivered_at >= created_at)
);

CREATE FUNCTION dm_private.enforce_message_receipt_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    sender_actor_kind dm.actor_kind;
    sender_actor_id text;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF NEW.id IS DISTINCT FROM OLD.id
            OR NEW.message_id IS DISTINCT FROM OLD.message_id
            OR NEW.conversation_id IS DISTINCT FROM OLD.conversation_id
            OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
            OR NEW.recipient_kind IS DISTINCT FROM OLD.recipient_kind
            OR NEW.recipient_id IS DISTINCT FROM OLD.recipient_id
            OR NEW.device_id IS DISTINCT FROM OLD.device_id
            OR NEW.delivered_at IS DISTINCT FROM OLD.delivered_at
            OR NEW.created_at IS DISTINCT FROM OLD.created_at
        THEN
            RAISE EXCEPTION 'receipt identity and delivery timestamp are immutable'
                USING ERRCODE = '23514';
        END IF;

        IF OLD.status = 'read' AND NEW.status <> 'read' THEN
            RAISE EXCEPTION 'message receipt status cannot move backward'
                USING ERRCODE = '23514';
        END IF;

        IF OLD.status = 'read' AND NEW.read_at IS DISTINCT FROM OLD.read_at THEN
            RAISE EXCEPTION 'message receipt read timestamp is immutable once set'
                USING ERRCODE = '23514';
        END IF;
    END IF;

    IF NEW.status = 'read' AND NEW.read_at IS NULL THEN
        NEW.read_at := GREATEST(NEW.delivered_at, clock_timestamp());
    END IF;

    SELECT sender_kind, sender_id
    INTO sender_actor_kind, sender_actor_id
    FROM dm.messages
    WHERE id = NEW.message_id;

    IF sender_actor_kind = NEW.recipient_kind AND sender_actor_id = NEW.recipient_id THEN
        RAISE EXCEPTION 'message sender cannot acknowledge their own message'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE FUNCTION dm_private.advance_message_aggregate_status()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    all_delivered boolean;
    all_read boolean;
BEGIN
    -- Serialize aggregate evaluation for every device/recipient of this message.
    -- The SELECTs below execute after the row lock is acquired, so a waiter
    -- observes receipts committed by the transaction that released the lock.
    PERFORM 1
    FROM dm.messages
    WHERE id = NEW.message_id
    FOR UPDATE;

    SELECT
        count(*) > 0
            AND bool_and(EXISTS (
                SELECT 1
                FROM dm.message_receipts AS receipt
                WHERE receipt.message_id = NEW.message_id
                  AND receipt.recipient_kind = participant.actor_kind
                  AND receipt.recipient_id = participant.actor_id
                  AND receipt.status IN ('delivered', 'read')
            )),
        count(*) > 0
            AND bool_and(EXISTS (
                SELECT 1
                FROM dm.message_receipts AS receipt
                WHERE receipt.message_id = NEW.message_id
                  AND receipt.recipient_kind = participant.actor_kind
                  AND receipt.recipient_id = participant.actor_id
                  AND receipt.status = 'read'
            ))
    INTO all_delivered, all_read
    FROM dm.conversation_participants AS participant
    JOIN dm.messages AS message ON message.id = NEW.message_id
    WHERE participant.conversation_id = message.conversation_id
      AND (participant.actor_kind, participant.actor_id)
          <> (message.sender_kind, message.sender_id);

    IF all_read THEN
        UPDATE dm.messages
        SET status = 'read',
            delivered_at = COALESCE(delivered_at, clock_timestamp()),
            read_at = COALESCE(read_at, GREATEST(delivered_at, clock_timestamp()))
        WHERE id = NEW.message_id
          AND status NOT IN ('read', 'failed');
    ELSIF all_delivered THEN
        UPDATE dm.messages
        SET status = 'delivered',
            delivered_at = COALESCE(delivered_at, clock_timestamp())
        WHERE id = NEW.message_id
          AND status IN ('waiting', 'sent');
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER message_receipts_enforce_policy
BEFORE INSERT OR UPDATE ON dm.message_receipts
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_message_receipt_policy();

CREATE TRIGGER message_receipts_set_updated_at
BEFORE UPDATE ON dm.message_receipts
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE TRIGGER message_receipts_advance_aggregate
AFTER INSERT OR UPDATE ON dm.message_receipts
FOR EACH ROW EXECUTE FUNCTION dm_private.advance_message_aggregate_status();

CREATE INDEX message_receipts_aggregate_idx
    ON dm.message_receipts (
        message_id,
        recipient_kind,
        recipient_id,
        status
    );
CREATE INDEX message_receipts_actor_idx
    ON dm.message_receipts (
        organization_id,
        recipient_kind,
        recipient_id,
        updated_at DESC
    );

CREATE TABLE dm.message_bundles (
    id uuid PRIMARY KEY,
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    created_by_kind dm.actor_kind NOT NULL,
    created_by_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    sealed_at timestamptz,
    UNIQUE (id, conversation_id, organization_id),
    CONSTRAINT message_bundles_conversation_fk
        FOREIGN KEY (conversation_id, organization_id)
        REFERENCES dm.conversations (id, organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT message_bundles_creator_participant_fk
        FOREIGN KEY (conversation_id, organization_id, created_by_kind, created_by_id)
        REFERENCES dm.conversation_participants (
            conversation_id,
            organization_id,
            actor_kind,
            actor_id
        )
        ON DELETE RESTRICT,
    CONSTRAINT message_bundles_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT message_bundles_silicon_creator CHECK (created_by_kind = 'silicon'),
    CONSTRAINT message_bundles_seal_order CHECK (
        sealed_at IS NULL OR sealed_at >= created_at
    )
);

CREATE INDEX message_bundles_conversation_idx
    ON dm.message_bundles (conversation_id, created_at DESC, id DESC);

CREATE FUNCTION dm_private.enforce_message_bundle_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.conversation_id IS DISTINCT FROM OLD.conversation_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.created_by_kind IS DISTINCT FROM OLD.created_by_kind
        OR NEW.created_by_id IS DISTINCT FROM OLD.created_by_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'bundle identity and creator are immutable'
            USING ERRCODE = '23514';
    END IF;

    IF OLD.sealed_at IS NOT NULL AND NEW.sealed_at IS DISTINCT FROM OLD.sealed_at THEN
        RAISE EXCEPTION 'bundle seal is immutable once set'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER message_bundles_enforce_update_policy
BEFORE UPDATE ON dm.message_bundles
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_message_bundle_update_policy();

CREATE TABLE dm.message_bundle_items (
    bundle_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    message_id uuid NOT NULL,
    role dm.bundle_role NOT NULL,
    position smallint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (bundle_id, message_id),
    UNIQUE (message_id),
    CONSTRAINT message_bundle_items_bundle_fk
        FOREIGN KEY (bundle_id, conversation_id, organization_id)
        REFERENCES dm.message_bundles (id, conversation_id, organization_id)
        ON DELETE CASCADE,
    CONSTRAINT message_bundle_items_message_fk
        FOREIGN KEY (message_id, conversation_id, organization_id)
        REFERENCES dm.messages (id, conversation_id, organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT message_bundle_items_position CHECK (
        (role = 'display' AND position = 0)
        OR (role = 'member' AND position BETWEEN 1 AND 100)
    )
);

CREATE FUNCTION dm_private.guard_message_bundle_item_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_bundle_id uuid;
    bundle_sealed_at timestamptz;
BEGIN
    IF TG_OP = 'DELETE' THEN
        target_bundle_id := OLD.bundle_id;
    ELSE
        target_bundle_id := NEW.bundle_id;
    END IF;

    SELECT sealed_at
    INTO bundle_sealed_at
    FROM dm.message_bundles
    WHERE id = target_bundle_id
    FOR UPDATE;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'bundle % does not exist', target_bundle_id
            USING ERRCODE = '23503';
    END IF;

    IF bundle_sealed_at IS NOT NULL THEN
        RAISE EXCEPTION 'bundle items are sealed and immutable'
            USING ERRCODE = '23514';
    END IF;

    IF TG_OP = 'UPDATE' AND NEW.bundle_id IS DISTINCT FROM OLD.bundle_id THEN
        RAISE EXCEPTION 'bundle items cannot move between bundles'
            USING ERRCODE = '23514';
    END IF;

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER message_bundle_items_guard_mutation
BEFORE INSERT OR UPDATE OR DELETE ON dm.message_bundle_items
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_message_bundle_item_mutation();

CREATE UNIQUE INDEX message_bundle_items_one_display_idx
    ON dm.message_bundle_items (bundle_id)
    WHERE role = 'display';
CREATE UNIQUE INDEX message_bundle_items_member_position_idx
    ON dm.message_bundle_items (bundle_id, position)
    WHERE role = 'member';
CREATE INDEX message_bundle_items_bundle_role_idx
    ON dm.message_bundle_items (bundle_id, role, position);

CREATE FUNCTION dm_private.assert_bundle_shape(p_bundle_id uuid)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    bundle_row dm.message_bundles%ROWTYPE;
    display_count bigint;
    member_count bigint;
    display_sequence bigint;
    maximum_member_sequence bigint;
    display_sender_matches boolean;
BEGIN
    SELECT *
    INTO bundle_row
    FROM dm.message_bundles
    WHERE id = p_bundle_id
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    SELECT
        count(*) FILTER (WHERE item.role = 'display'),
        count(*) FILTER (WHERE item.role = 'member'),
        max(message.sequence) FILTER (WHERE item.role = 'display'),
        max(message.sequence) FILTER (WHERE item.role = 'member'),
        bool_and(
            item.role <> 'display'
            OR (
                message.sender_kind = bundle_row.created_by_kind
                AND message.sender_id = bundle_row.created_by_id
            )
        )
    INTO
        display_count,
        member_count,
        display_sequence,
        maximum_member_sequence,
        display_sender_matches
    FROM dm.message_bundle_items AS item
    JOIN dm.messages AS message ON message.id = item.message_id
    WHERE item.bundle_id = p_bundle_id;

    IF display_count <> 1 OR member_count NOT BETWEEN 1 AND 100 THEN
        RAISE EXCEPTION 'bundle % requires one display and between one and 100 members', p_bundle_id
            USING ERRCODE = '23514';
    END IF;

    IF NOT display_sender_matches THEN
        RAISE EXCEPTION 'bundle % display message must be authored by its Silicon creator', p_bundle_id
            USING ERRCODE = '23514';
    END IF;

    IF display_sequence <= maximum_member_sequence THEN
        RAISE EXCEPTION 'bundle % display message must be appended after every member', p_bundle_id
            USING ERRCODE = '23514';
    END IF;

    UPDATE dm.message_bundles
    SET sealed_at = transaction_timestamp()
    WHERE id = p_bundle_id
      AND sealed_at IS NULL;
END;
$$;

CREATE FUNCTION dm_private.check_bundle_shape_from_bundle()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM dm_private.assert_bundle_shape(NEW.id);
    RETURN NEW;
END;
$$;

CREATE FUNCTION dm_private.check_bundle_shape_from_item()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM dm_private.assert_bundle_shape(OLD.bundle_id);
        RETURN OLD;
    END IF;

    PERFORM dm_private.assert_bundle_shape(NEW.bundle_id);
    IF TG_OP = 'UPDATE' AND NEW.bundle_id IS DISTINCT FROM OLD.bundle_id THEN
        PERFORM dm_private.assert_bundle_shape(OLD.bundle_id);
    END IF;
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER message_bundles_require_shape
AFTER INSERT ON dm.message_bundles
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_bundle_shape_from_bundle();

CREATE CONSTRAINT TRIGGER message_bundle_items_require_shape
AFTER INSERT OR UPDATE OR DELETE ON dm.message_bundle_items
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_bundle_shape_from_item();

CREATE TABLE dm.drafts (
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    version bigint NOT NULL DEFAULT 1,
    text_content text,
    content_hash bytea NOT NULL,
    content_mutation_txid bigint NOT NULL DEFAULT txid_current(),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (conversation_id, actor_kind, actor_id),
    UNIQUE (conversation_id, organization_id, actor_kind, actor_id),
    CONSTRAINT drafts_actor_participant_fk
        FOREIGN KEY (conversation_id, organization_id, actor_kind, actor_id)
        REFERENCES dm.conversation_participants (
            conversation_id,
            organization_id,
            actor_kind,
            actor_id
        )
        ON DELETE CASCADE,
    CONSTRAINT drafts_positive_version CHECK (version > 0),
    CONSTRAINT drafts_text_length
        CHECK (text_content IS NULL OR char_length(text_content) <= 100000000),
    CONSTRAINT drafts_content_hash_length CHECK (octet_length(content_hash) = 32),
    CONSTRAINT drafts_positive_content_mutation_txid CHECK (content_mutation_txid > 0)
);

COMMENT ON TABLE dm.drafts IS
    'One synchronized, optimistically versioned draft per conversation participant.';

CREATE FUNCTION dm_private.enforce_draft_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.conversation_id IS DISTINCT FROM OLD.conversation_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.actor_kind IS DISTINCT FROM OLD.actor_kind
        OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'draft ownership is immutable' USING ERRCODE = '23514';
    END IF;

    IF NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'draft version must advance by exactly one'
            USING ERRCODE = '23514';
    END IF;

    NEW.content_mutation_txid := txid_current();
    NEW.updated_at := transaction_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER drafts_enforce_update_policy
BEFORE UPDATE ON dm.drafts
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_draft_update_policy();

CREATE INDEX drafts_actor_idx
    ON dm.drafts (organization_id, actor_kind, actor_id, updated_at DESC);

CREATE TABLE dm.draft_attachments (
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    position smallint NOT NULL,
    attachment_kind dm.attachment_kind NOT NULL DEFAULT 'attachment',
    permanent_url text NOT NULL,
    name text,
    content_type text,
    declared_size_bytes bigint,
    duration_milliseconds bigint,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (conversation_id, actor_kind, actor_id, position),
    CONSTRAINT draft_attachments_draft_fk
        FOREIGN KEY (conversation_id, organization_id, actor_kind, actor_id)
        REFERENCES dm.drafts (conversation_id, organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT,
    CONSTRAINT draft_attachments_position CHECK (
        (attachment_kind = 'attachment' AND position BETWEEN 0 AND 99)
        OR (attachment_kind = 'voice' AND position = 100)
    ),
    CONSTRAINT draft_attachments_https_url CHECK (
        char_length(permanent_url) BETWEEN 9 AND 8192
        AND permanent_url ~ '^https://[^[:space:]]+$'
    ),
    CONSTRAINT draft_attachments_name_length
        CHECK (name IS NULL OR char_length(name) BETWEEN 1 AND 1024),
    CONSTRAINT draft_attachments_content_type_length
        CHECK (content_type IS NULL OR char_length(content_type) BETWEEN 1 AND 255),
    CONSTRAINT draft_attachments_size_limit CHECK (
        declared_size_bytes IS NULL
        OR declared_size_bytes BETWEEN 0 AND 5368709120
    ),
    CONSTRAINT draft_attachments_duration_limit CHECK (
        duration_milliseconds IS NULL
        OR duration_milliseconds BETWEEN 0 AND 172800000
    ),
    CONSTRAINT draft_attachments_duration_kind CHECK (
        attachment_kind = 'voice' OR duration_milliseconds IS NULL
    )
);

CREATE UNIQUE INDEX draft_attachments_one_voice_idx
    ON dm.draft_attachments (conversation_id, actor_kind, actor_id)
    WHERE attachment_kind = 'voice';

CREATE TABLE dm.draft_gifs (
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    provider_id text NOT NULL,
    url text NOT NULL,
    preview_url text,
    title text,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (conversation_id, actor_kind, actor_id),
    CONSTRAINT draft_gifs_draft_fk
        FOREIGN KEY (conversation_id, organization_id, actor_kind, actor_id)
        REFERENCES dm.drafts (conversation_id, organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT,
    CONSTRAINT draft_gifs_provider_id_length CHECK (char_length(provider_id) BETWEEN 1 AND 255),
    CONSTRAINT draft_gifs_url CHECK (
        char_length(url) BETWEEN 9 AND 8192
        AND url ~ '^https://[^[:space:]]+$'
    ),
    CONSTRAINT draft_gifs_preview_url CHECK (
        preview_url IS NULL
        OR (
            char_length(preview_url) BETWEEN 9 AND 8192
            AND preview_url ~ '^https://[^[:space:]]+$'
        )
    ),
    CONSTRAINT draft_gifs_title_length CHECK (title IS NULL OR char_length(title) <= 1000)
);

CREATE FUNCTION dm_private.guard_draft_content_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    parent_content_txid bigint;
    target_conversation_id uuid;
    target_organization_id text;
    target_actor_kind dm.actor_kind;
    target_actor_id text;
BEGIN
    IF TG_OP = 'UPDATE' AND (
        NEW.conversation_id IS DISTINCT FROM OLD.conversation_id
        OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.actor_kind IS DISTINCT FROM OLD.actor_kind
        OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    ) THEN
        RAISE EXCEPTION 'draft content ownership and creation time are immutable'
            USING ERRCODE = '23514';
    END IF;

    IF TG_OP = 'DELETE' THEN
        target_conversation_id := OLD.conversation_id;
        target_organization_id := OLD.organization_id;
        target_actor_kind := OLD.actor_kind;
        target_actor_id := OLD.actor_id;
    ELSE
        target_conversation_id := NEW.conversation_id;
        target_organization_id := NEW.organization_id;
        target_actor_kind := NEW.actor_kind;
        target_actor_id := NEW.actor_id;
    END IF;

    SELECT content_mutation_txid
    INTO parent_content_txid
    FROM dm.drafts
    WHERE conversation_id = target_conversation_id
      AND organization_id = target_organization_id
      AND actor_kind = target_actor_kind
      AND actor_id = target_actor_id
    FOR UPDATE;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'owning draft does not exist' USING ERRCODE = '23503';
    END IF;

    IF parent_content_txid <> txid_current() THEN
        RAISE EXCEPTION 'draft content mutation requires a versioned draft update in this transaction'
            USING ERRCODE = '23514';
    END IF;

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER draft_attachments_guard_content_mutation
BEFORE INSERT OR UPDATE OR DELETE ON dm.draft_attachments
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_draft_content_mutation();

CREATE TRIGGER draft_gifs_guard_content_mutation
BEFORE INSERT OR UPDATE OR DELETE ON dm.draft_gifs
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_draft_content_mutation();

CREATE FUNCTION dm_private.assert_draft_attachment_limit(
    p_conversation_id uuid,
    p_actor_kind dm.actor_kind,
    p_actor_id text
)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    attachment_count bigint;
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM dm.drafts
        WHERE conversation_id = p_conversation_id
          AND actor_kind = p_actor_kind
          AND actor_id = p_actor_id
    ) THEN
        RETURN;
    END IF;

    SELECT count(*)
    INTO attachment_count
    FROM dm.draft_attachments
    WHERE conversation_id = p_conversation_id
      AND actor_kind = p_actor_kind
      AND actor_id = p_actor_id;

    IF attachment_count > 100 THEN
        RAISE EXCEPTION 'draft cannot contain more than 100 combined attachments and voice items'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

CREATE FUNCTION dm_private.check_draft_attachment_limit_from_draft()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM dm_private.assert_draft_attachment_limit(
        NEW.conversation_id,
        NEW.actor_kind,
        NEW.actor_id
    );
    RETURN NEW;
END;
$$;

CREATE FUNCTION dm_private.check_draft_attachment_limit_from_child()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM dm_private.assert_draft_attachment_limit(
            OLD.conversation_id,
            OLD.actor_kind,
            OLD.actor_id
        );
        RETURN OLD;
    END IF;

    PERFORM dm_private.assert_draft_attachment_limit(
        NEW.conversation_id,
        NEW.actor_kind,
        NEW.actor_id
    );
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER drafts_validate_attachment_limit
AFTER INSERT OR UPDATE ON dm.drafts
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_draft_attachment_limit_from_draft();

CREATE CONSTRAINT TRIGGER draft_attachments_validate_limit
AFTER INSERT OR UPDATE OR DELETE ON dm.draft_attachments
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_draft_attachment_limit_from_child();

CREATE TABLE dm.recent_gifs (
    organization_id text NOT NULL,
    carbon_kind dm.actor_kind GENERATED ALWAYS AS ('carbon'::dm.actor_kind) STORED,
    carbon_id text NOT NULL,
    provider_id text NOT NULL,
    url text NOT NULL,
    preview_url text,
    title text,
    last_message_id uuid NOT NULL,
    first_used_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    last_used_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, carbon_id, provider_id),
    CONSTRAINT recent_gifs_carbon_fk
        FOREIGN KEY (organization_id, carbon_kind, carbon_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE CASCADE,
    CONSTRAINT recent_gifs_message_fk
        FOREIGN KEY (last_message_id, organization_id)
        REFERENCES dm.messages (id, organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT recent_gifs_provider_id_length CHECK (char_length(provider_id) BETWEEN 1 AND 255),
    CONSTRAINT recent_gifs_url CHECK (
        char_length(url) BETWEEN 9 AND 8192
        AND url ~ '^https://[^[:space:]]+$'
    ),
    CONSTRAINT recent_gifs_preview_url CHECK (
        preview_url IS NULL
        OR (
            char_length(preview_url) BETWEEN 9 AND 8192
            AND preview_url ~ '^https://[^[:space:]]+$'
        )
    ),
    CONSTRAINT recent_gifs_title_length CHECK (title IS NULL OR char_length(title) <= 1000),
    CONSTRAINT recent_gifs_usage_order CHECK (last_used_at >= first_used_at)
);

CREATE FUNCTION dm_private.enforce_recent_gif_source_and_limit()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    source_matches boolean;
BEGIN
    SELECT EXISTS (
        SELECT 1
        FROM dm.messages AS message
        JOIN dm.message_gifs AS gif ON gif.message_id = message.id
        WHERE message.id = NEW.last_message_id
          AND message.organization_id = NEW.organization_id
          AND message.sender_kind = 'carbon'
          AND message.sender_id = NEW.carbon_id
          AND gif.provider_id = NEW.provider_id
          AND gif.url = NEW.url
          AND gif.preview_url IS NOT DISTINCT FROM NEW.preview_url
          AND gif.title IS NOT DISTINCT FROM NEW.title
    ) INTO source_matches;

    IF NOT source_matches THEN
        RAISE EXCEPTION 'recent GIF must match a GIF sent by the same Carbon'
            USING ERRCODE = '23514';
    END IF;

    PERFORM 1
    FROM dm.actor_snapshots
    WHERE organization_id = NEW.organization_id
      AND actor_kind = 'carbon'
      AND actor_id = NEW.carbon_id
    FOR UPDATE;

    DELETE FROM dm.recent_gifs AS stale
    WHERE stale.organization_id = NEW.organization_id
      AND stale.carbon_id = NEW.carbon_id
      AND stale.provider_id IN (
          SELECT candidate.provider_id
          FROM dm.recent_gifs AS candidate
          WHERE candidate.organization_id = NEW.organization_id
            AND candidate.carbon_id = NEW.carbon_id
          ORDER BY candidate.last_used_at DESC, candidate.provider_id DESC
          OFFSET 20
      );

    RETURN NEW;
END;
$$;

CREATE TRIGGER recent_gifs_enforce_source_and_limit
AFTER INSERT OR UPDATE ON dm.recent_gifs
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_recent_gif_source_and_limit();

CREATE INDEX recent_gifs_recency_idx
    ON dm.recent_gifs (
        organization_id,
        carbon_id,
        last_used_at DESC,
        provider_id DESC
    );

CREATE TABLE dm.system_events (
    event_id uuid PRIMARY KEY,
    organization_id text NOT NULL,
    target_kind dm.actor_kind GENERATED ALWAYS AS ('silicon'::dm.actor_kind) STORED,
    target_silicon_id text NOT NULL,
    event_type text NOT NULL,
    trace_id text,
    payload jsonb NOT NULL,
    accepted_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    outbox_enqueued_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (event_id, organization_id),
    CONSTRAINT system_events_target_fk
        FOREIGN KEY (organization_id, target_kind, target_silicon_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT,
    CONSTRAINT system_events_non_nil_id
        CHECK (event_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT system_events_type_length
        CHECK (char_length(event_type) BETWEEN 1 AND 255),
    CONSTRAINT system_events_type_canonical CHECK (
        event_type = btrim(event_type) AND event_type !~ '[[:cntrl:]]'
    ),
    CONSTRAINT system_events_trace_id_length
        CHECK (trace_id IS NULL OR char_length(trace_id) BETWEEN 1 AND 255),
    CONSTRAINT system_events_payload_object CHECK (jsonb_typeof(payload) = 'object'),
    CONSTRAINT system_events_payload_size
        CHECK (octet_length(payload::text) <= 1048576),
    CONSTRAINT system_events_outbox_enqueue_order
        CHECK (outbox_enqueued_at >= created_at)
);

COMMENT ON TABLE dm.system_events IS
    'Idempotently accepted, immutable Silicon Hook events retained independently of delivery attempts.';

CREATE FUNCTION dm_private.enforce_system_event_immutability()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'system events are immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER system_events_enforce_immutability
BEFORE UPDATE ON dm.system_events
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_system_event_immutability();

CREATE INDEX system_events_target_idx
    ON dm.system_events (
        organization_id,
        target_silicon_id,
        accepted_at DESC,
        event_id DESC
    );
CREATE INDEX system_events_trace_idx
    ON dm.system_events (trace_id)
    WHERE trace_id IS NOT NULL;
