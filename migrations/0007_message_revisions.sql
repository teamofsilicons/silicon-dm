-- Edits are append-only versions; original accepted messages remain durable.
CREATE TABLE dm.message_revisions (
    id uuid PRIMARY KEY,
    message_id uuid NOT NULL REFERENCES dm.messages(id),
    version bigint NOT NULL CHECK (version >= 2),
    content jsonb,
    deleted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (message_id, version),
    CHECK ((content IS NOT NULL AND jsonb_typeof(content) = 'object' AND deleted_at IS NULL)
        OR (content IS NULL AND deleted_at IS NOT NULL))
);
ALTER TABLE dm.actor_deliveries ADD COLUMN delivery_revision bigint NOT NULL DEFAULT 1
    CHECK (delivery_revision > 0);
DROP INDEX dm.actor_deliveries_one_message_per_target_idx;
CREATE UNIQUE INDEX actor_deliveries_one_message_per_target_idx ON dm.actor_deliveries
    (organization_id, target_kind, target_id, message_id, delivery_revision)
    WHERE delivery_kind = 'message';

CREATE OR REPLACE FUNCTION dm_private.assign_actor_delivery_sequence()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    allocated_sequence bigint;
BEGIN
    INSERT INTO dm.actor_delivery_streams (organization_id, actor_kind, actor_id)
    VALUES (NEW.organization_id, NEW.target_kind, NEW.target_id)
    ON CONFLICT (organization_id, actor_kind, actor_id) DO NOTHING;

    PERFORM 1
    FROM dm.actor_delivery_streams
    WHERE organization_id = NEW.organization_id
      AND actor_kind = NEW.target_kind
      AND actor_id = NEW.target_id
    FOR UPDATE;

    IF EXISTS (SELECT 1 FROM dm.actor_deliveries WHERE id = NEW.id) THEN
        RAISE EXCEPTION 'actor delivery ID % already exists', NEW.id
            USING ERRCODE = '23505';
    END IF;

    IF (
        NEW.delivery_kind = 'message'
        AND EXISTS (
            SELECT 1
            FROM dm.actor_deliveries
            WHERE organization_id = NEW.organization_id
              AND target_kind = NEW.target_kind
              AND target_id = NEW.target_id
              AND delivery_kind = 'message'
              AND message_id = NEW.message_id
              AND delivery_revision = NEW.delivery_revision
        )
    ) OR (
        NEW.delivery_kind = 'message_status'
        AND EXISTS (
            SELECT 1
            FROM dm.actor_deliveries
            WHERE organization_id = NEW.organization_id
              AND target_kind = NEW.target_kind
              AND target_id = NEW.target_id
              AND delivery_kind = 'message_status'
              AND message_id = NEW.message_id
              AND aggregate_status = NEW.aggregate_status
        )
    ) OR (
        NEW.delivery_kind = 'system_event'
        AND EXISTS (
            SELECT 1
            FROM dm.actor_deliveries
            WHERE organization_id = NEW.organization_id
              AND target_kind = NEW.target_kind
              AND target_id = NEW.target_id
              AND delivery_kind = 'system_event'
              AND system_event_id = NEW.system_event_id
        )
    ) THEN
        -- Returning NULL from a BEFORE ROW trigger makes idempotent source
        -- re-enqueue a no-op before the actor sequence can be consumed.
        RETURN NULL;
    END IF;

    UPDATE dm.actor_delivery_streams
    SET next_sequence = next_sequence + 1
    WHERE organization_id = NEW.organization_id
      AND actor_kind = NEW.target_kind
      AND actor_id = NEW.target_id
    RETURNING next_sequence - 1 INTO allocated_sequence;

    IF allocated_sequence IS NULL THEN
        RAISE EXCEPTION 'delivery target actor does not exist' USING ERRCODE = '23503';
    END IF;

    NEW.sequence := allocated_sequence;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION dm_private.enforce_actor_delivery_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    expected_kind dm.actor_kind;
    expected_id text;
    current_message_status dm.message_status;
    current_receipt_status dm.receipt_status;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF NEW.id IS DISTINCT FROM OLD.id
            OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
            OR NEW.target_kind IS DISTINCT FROM OLD.target_kind
            OR NEW.target_id IS DISTINCT FROM OLD.target_id
            OR NEW.sequence IS DISTINCT FROM OLD.sequence
            OR NEW.delivery_kind IS DISTINCT FROM OLD.delivery_kind
            OR NEW.conversation_id IS DISTINCT FROM OLD.conversation_id
            OR NEW.delivery_revision IS DISTINCT FROM OLD.delivery_revision
            OR NEW.message_id IS DISTINCT FROM OLD.message_id
            OR NEW.receipt_id IS DISTINCT FROM OLD.receipt_id
            OR NEW.aggregate_status IS DISTINCT FROM OLD.aggregate_status
            OR NEW.system_event_id IS DISTINCT FROM OLD.system_event_id
            OR NEW.created_at IS DISTINCT FROM OLD.created_at
        THEN
            RAISE EXCEPTION 'actor delivery identity and source are immutable'
                USING ERRCODE = '23514';
        END IF;

        IF NEW.attempt_count NOT IN (OLD.attempt_count, OLD.attempt_count + 1) THEN
            RAISE EXCEPTION 'delivery attempt count must advance by at most one'
                USING ERRCODE = '23514';
        END IF;
        IF OLD.acked_at IS NOT NULL AND NEW.acked_at IS DISTINCT FROM OLD.acked_at THEN
            RAISE EXCEPTION 'delivery ACK timestamp cannot be changed or cleared'
                USING ERRCODE = '23514';
        END IF;
        IF OLD.dead_lettered_at IS NOT NULL
            AND NEW.dead_lettered_at IS DISTINCT FROM OLD.dead_lettered_at
        THEN
            RAISE EXCEPTION 'delivery dead-letter timestamp cannot be changed or cleared'
                USING ERRCODE = '23514';
        END IF;
        IF (OLD.acked_at IS NOT NULL OR OLD.dead_lettered_at IS NOT NULL) AND (
            NEW.next_attempt_at IS DISTINCT FROM OLD.next_attempt_at
            OR NEW.attempt_count IS DISTINCT FROM OLD.attempt_count
            OR NEW.lease_owner IS DISTINCT FROM OLD.lease_owner
            OR NEW.lease_started_at IS DISTINCT FROM OLD.lease_started_at
            OR NEW.lease_expires_at IS DISTINCT FROM OLD.lease_expires_at
            OR NEW.last_attempt_at IS DISTINCT FROM OLD.last_attempt_at
            OR NEW.last_error_code IS DISTINCT FROM OLD.last_error_code
            OR NEW.retain_until IS DISTINCT FROM OLD.retain_until
        ) THEN
            RAISE EXCEPTION 'terminal actor deliveries are immutable'
                USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;

    IF NEW.attempt_count <> 0
        OR NEW.lease_owner IS NOT NULL
        OR NEW.lease_started_at IS NOT NULL
        OR NEW.lease_expires_at IS NOT NULL
        OR NEW.last_attempt_at IS NOT NULL
        OR NEW.last_error_code IS NOT NULL
        OR NEW.acked_at IS NOT NULL
        OR NEW.retain_until IS NOT NULL
        OR NEW.dead_lettered_at IS NOT NULL
    THEN
        RAISE EXCEPTION 'new actor deliveries must begin in a pristine claimable state'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.delivery_kind = 'message' THEN
        IF NOT EXISTS (
            SELECT 1 FROM dm.conversation_participants
            WHERE conversation_id = NEW.conversation_id
              AND organization_id = NEW.organization_id
              AND actor_kind = NEW.target_kind AND actor_id = NEW.target_id
        ) THEN
            RAISE EXCEPTION 'message delivery target must be a participant' USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.delivery_kind = 'message_status' THEN
        SELECT sender_kind, sender_id, status
        INTO expected_kind, expected_id, current_message_status
        FROM dm.messages
        WHERE id = NEW.message_id;

        IF expected_kind IS DISTINCT FROM NEW.target_kind
            OR expected_id IS DISTINCT FROM NEW.target_id
        THEN
            RAISE EXCEPTION 'message-status delivery target must be the original message sender'
                USING ERRCODE = '23514';
        END IF;

        IF (NEW.aggregate_status = 'delivered' AND current_message_status NOT IN ('delivered', 'read'))
            OR (NEW.aggregate_status = 'read' AND current_message_status <> 'read')
            OR (NEW.aggregate_status = 'failed' AND current_message_status <> 'failed')
        THEN
            RAISE EXCEPTION 'message-status delivery cannot announce a state the message has not reached'
                USING ERRCODE = '23514';
        END IF;

        IF NEW.receipt_id IS NOT NULL THEN
            SELECT status
            INTO current_receipt_status
            FROM dm.message_receipts
            WHERE id = NEW.receipt_id
              AND message_id = NEW.message_id;

            IF NOT FOUND
                OR (NEW.aggregate_status = 'read' AND current_receipt_status <> 'read')
            THEN
                RAISE EXCEPTION 'causal receipt does not support the aggregate status snapshot'
                    USING ERRCODE = '23514';
            END IF;
        END IF;
    ELSE
        SELECT target_kind, target_silicon_id
        INTO expected_kind, expected_id
        FROM dm.system_events
        WHERE event_id = NEW.system_event_id;

        IF expected_kind IS DISTINCT FROM NEW.target_kind
            OR expected_id IS DISTINCT FROM NEW.target_id
        THEN
            RAISE EXCEPTION 'system event delivery target must match the Hook event target'
                USING ERRCODE = '23514';
        END IF;
    END IF;

    RETURN NEW;
END;
$$;
