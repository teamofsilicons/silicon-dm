-- Replayable actor delivery streams, WebSocket presence leases, and request idempotency.

CREATE TYPE dm.delivery_kind AS ENUM ('message', 'message_status', 'system_event');
CREATE TYPE dm.presence_activity AS ENUM (
    'typing',
    'recording_voice',
    'transcribing_voice',
    'uploading_file',
    'searching_gifs'
);
CREATE TYPE dm.idempotency_status AS ENUM ('in_progress', 'completed');

CREATE TABLE dm.actor_delivery_streams (
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    next_sequence bigint NOT NULL DEFAULT 1,
    last_acked_sequence bigint NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, actor_kind, actor_id),
    CONSTRAINT actor_delivery_streams_actor_fk
        FOREIGN KEY (organization_id, actor_kind, actor_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT,
    CONSTRAINT actor_delivery_streams_sequence_bounds CHECK (
        next_sequence > 0
        AND last_acked_sequence >= 0
        AND last_acked_sequence < next_sequence
    )
);

COMMENT ON TABLE dm.actor_delivery_streams IS
    'Row-locked allocator and cumulative server ACK watermark for one actor delivery stream.';

CREATE TRIGGER actor_delivery_streams_set_updated_at
BEFORE UPDATE ON dm.actor_delivery_streams
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE FUNCTION dm_private.enforce_actor_delivery_stream_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.actor_kind IS DISTINCT FROM OLD.actor_kind
        OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'actor delivery stream identity is immutable' USING ERRCODE = '23514';
    END IF;

    IF NEW.next_sequence NOT IN (OLD.next_sequence, OLD.next_sequence + 1) THEN
        RAISE EXCEPTION 'actor delivery sequence must advance by exactly one'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.last_acked_sequence < OLD.last_acked_sequence THEN
        RAISE EXCEPTION 'actor delivery ACK watermark cannot move backward'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER actor_delivery_streams_enforce_update_policy
BEFORE UPDATE ON dm.actor_delivery_streams
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_actor_delivery_stream_update_policy();

CREATE TABLE dm.actor_deliveries (
    id uuid PRIMARY KEY,
    organization_id text NOT NULL,
    target_kind dm.actor_kind NOT NULL,
    target_id text NOT NULL,
    sequence bigint NOT NULL,
    delivery_kind dm.delivery_kind NOT NULL,
    conversation_id uuid,
    message_id uuid,
    receipt_id uuid,
    aggregate_status dm.message_status,
    system_event_id uuid,
    next_attempt_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    attempt_count integer NOT NULL DEFAULT 0,
    lease_owner text,
    lease_started_at timestamptz,
    lease_expires_at timestamptz,
    last_attempt_at timestamptz,
    last_error_code text,
    acked_at timestamptz,
    retain_until timestamptz,
    dead_lettered_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (organization_id, target_kind, target_id, sequence),
    UNIQUE (id, organization_id),
    CONSTRAINT actor_deliveries_target_fk
        FOREIGN KEY (organization_id, target_kind, target_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT,
    CONSTRAINT actor_deliveries_target_participant_fk
        FOREIGN KEY (conversation_id, organization_id, target_kind, target_id)
        REFERENCES dm.conversation_participants (
            conversation_id,
            organization_id,
            actor_kind,
            actor_id
        )
        ON DELETE RESTRICT,
    CONSTRAINT actor_deliveries_message_fk
        FOREIGN KEY (message_id, conversation_id, organization_id)
        REFERENCES dm.messages (id, conversation_id, organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT actor_deliveries_receipt_fk
        FOREIGN KEY (receipt_id, message_id, conversation_id, organization_id)
        REFERENCES dm.message_receipts (
            id,
            message_id,
            conversation_id,
            organization_id
        )
        ON DELETE RESTRICT,
    CONSTRAINT actor_deliveries_system_event_fk
        FOREIGN KEY (system_event_id, organization_id)
        REFERENCES dm.system_events (event_id, organization_id)
        ON DELETE RESTRICT,
    CONSTRAINT actor_deliveries_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT actor_deliveries_positive_sequence CHECK (sequence > 0),
    CONSTRAINT actor_deliveries_source_consistency CHECK (
        (
            delivery_kind = 'message'
            AND conversation_id IS NOT NULL
            AND message_id IS NOT NULL
            AND receipt_id IS NULL
            AND aggregate_status IS NULL
            AND system_event_id IS NULL
        )
        OR (
            delivery_kind = 'message_status'
            AND conversation_id IS NOT NULL
            AND message_id IS NOT NULL
            AND aggregate_status IS NOT NULL
            AND aggregate_status IN ('delivered', 'read', 'failed')
            AND system_event_id IS NULL
            AND (aggregate_status <> 'failed' OR receipt_id IS NULL)
        )
        OR (
            delivery_kind = 'system_event'
            AND conversation_id IS NULL
            AND message_id IS NULL
            AND receipt_id IS NULL
            AND aggregate_status IS NULL
            AND system_event_id IS NOT NULL
        )
    ),
    CONSTRAINT actor_deliveries_nonnegative_attempt_count CHECK (attempt_count >= 0),
    CONSTRAINT actor_deliveries_lease_consistency CHECK (
        (
            lease_owner IS NULL
            AND lease_started_at IS NULL
            AND lease_expires_at IS NULL
        )
        OR (
            lease_owner IS NOT NULL
            AND lease_started_at IS NOT NULL
            AND lease_expires_at IS NOT NULL
            AND lease_expires_at > lease_started_at
        )
    ),
    CONSTRAINT actor_deliveries_lease_owner_length CHECK (
        lease_owner IS NULL OR char_length(lease_owner) BETWEEN 1 AND 255
    ),
    CONSTRAINT actor_deliveries_last_error_code_length CHECK (
        last_error_code IS NULL OR char_length(last_error_code) BETWEEN 1 AND 255
    ),
    CONSTRAINT actor_deliveries_terminal_state_consistency CHECK (
        NOT (acked_at IS NOT NULL AND dead_lettered_at IS NOT NULL)
        AND (
            (acked_at IS NULL AND dead_lettered_at IS NULL)
            OR (lease_owner IS NULL AND lease_started_at IS NULL AND lease_expires_at IS NULL)
        )
    ),
    CONSTRAINT actor_deliveries_retention_consistency CHECK (
        (retain_until IS NULL AND acked_at IS NULL)
        OR (retain_until IS NOT NULL AND acked_at IS NOT NULL AND retain_until > acked_at)
    ),
    CONSTRAINT actor_deliveries_attempt_time_order CHECK (
        last_attempt_at IS NULL OR last_attempt_at >= created_at
    ),
    CONSTRAINT actor_deliveries_next_attempt_order CHECK (next_attempt_at >= created_at),
    CONSTRAINT actor_deliveries_terminal_time_order CHECK (
        (acked_at IS NULL OR acked_at >= created_at)
        AND (dead_lettered_at IS NULL OR dead_lettered_at >= created_at)
    )
);

COMMENT ON TABLE dm.actor_deliveries IS
    'Durable per-actor delivery outbox; leases are transient and ACKed rows remain replayable until retention expiry.';
COMMENT ON COLUMN dm.actor_deliveries.aggregate_status IS
    'Immutable aggregate message-state snapshot for the public receipt frame; distinct from an inbound device receipt.';

CREATE FUNCTION dm_private.assign_actor_delivery_sequence()
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

CREATE FUNCTION dm_private.enforce_actor_delivery_policy()
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
        SELECT sender_kind, sender_id
        INTO expected_kind, expected_id
        FROM dm.messages
        WHERE id = NEW.message_id;

        IF expected_kind = NEW.target_kind AND expected_id = NEW.target_id THEN
            RAISE EXCEPTION 'message delivery target cannot be the message sender'
                USING ERRCODE = '23514';
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

CREATE TRIGGER actor_deliveries_assign_sequence
BEFORE INSERT ON dm.actor_deliveries
FOR EACH ROW EXECUTE FUNCTION dm_private.assign_actor_delivery_sequence();

CREATE TRIGGER actor_deliveries_enforce_policy
BEFORE INSERT OR UPDATE ON dm.actor_deliveries
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_actor_delivery_policy();

CREATE TRIGGER actor_deliveries_set_updated_at
BEFORE UPDATE ON dm.actor_deliveries
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE FUNCTION dm_private.guard_actor_delivery_retention_delete()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.acked_at IS NULL
        OR OLD.retain_until IS NULL
        OR OLD.retain_until > transaction_timestamp()
    THEN
        RAISE EXCEPTION 'actor delivery can be deleted only after ACK retention expires'
            USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END;
$$;

CREATE TRIGGER actor_deliveries_guard_retention_delete
BEFORE DELETE ON dm.actor_deliveries
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_actor_delivery_retention_delete();

CREATE UNIQUE INDEX actor_deliveries_one_message_per_target_idx
    ON dm.actor_deliveries (
        organization_id,
        target_kind,
        target_id,
        message_id
    )
    WHERE delivery_kind = 'message';
CREATE UNIQUE INDEX actor_deliveries_one_message_status_per_target_idx
    ON dm.actor_deliveries (
        organization_id,
        target_kind,
        target_id,
        message_id,
        aggregate_status
    )
    WHERE delivery_kind = 'message_status';
CREATE UNIQUE INDEX actor_deliveries_one_event_per_target_idx
    ON dm.actor_deliveries (
        organization_id,
        target_kind,
        target_id,
        system_event_id
    )
    WHERE delivery_kind = 'system_event';
CREATE INDEX actor_deliveries_claim_idx
    ON dm.actor_deliveries (next_attempt_at, lease_expires_at, created_at, id)
    WHERE acked_at IS NULL AND dead_lettered_at IS NULL;
CREATE INDEX actor_deliveries_actor_replay_idx
    ON dm.actor_deliveries (
        organization_id,
        target_kind,
        target_id,
        sequence
    );
CREATE INDEX actor_deliveries_retention_idx
    ON dm.actor_deliveries (retain_until, id)
    WHERE acked_at IS NOT NULL;
CREATE INDEX actor_deliveries_dead_letter_idx
    ON dm.actor_deliveries (dead_lettered_at, id)
    WHERE dead_lettered_at IS NOT NULL;

CREATE FUNCTION dm_private.assert_message_outbox_complete(p_message_id uuid)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    source_message dm.messages%ROWTYPE;
BEGIN
    SELECT *
    INTO source_message
    FROM dm.messages
    WHERE id = p_message_id;

    IF NOT FOUND THEN
        RETURN;
    END IF;

    IF EXISTS (
        SELECT 1
        FROM dm.conversation_participants AS participant
        WHERE participant.conversation_id = source_message.conversation_id
          AND (participant.actor_kind, participant.actor_id)
              <> (source_message.sender_kind, source_message.sender_id)
          AND NOT EXISTS (
              SELECT 1
              FROM dm.actor_deliveries AS delivery
              WHERE delivery.organization_id = source_message.organization_id
                AND delivery.target_kind = participant.actor_kind
                AND delivery.target_id = participant.actor_id
                AND delivery.delivery_kind = 'message'
                AND delivery.message_id = source_message.id
                AND delivery.attempt_count = 0
                AND delivery.lease_owner IS NULL
                AND delivery.acked_at IS NULL
                AND delivery.dead_lettered_at IS NULL
          )
    ) THEN
        RAISE EXCEPTION 'message % must atomically enqueue one pristine delivery per recipient',
            p_message_id
            USING ERRCODE = '23514';
    END IF;
END;
$$;

CREATE FUNCTION dm_private.check_message_outbox_from_source()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM dm_private.assert_message_outbox_complete(NEW.id);
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER messages_require_complete_outbox
AFTER INSERT ON dm.messages
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_message_outbox_from_source();

CREATE FUNCTION dm_private.check_message_status_outbox()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.status IS NOT DISTINCT FROM OLD.status
        OR NEW.status NOT IN ('delivered', 'read', 'failed')
    THEN
        RETURN NEW;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM dm.actor_deliveries AS delivery
        WHERE delivery.organization_id = NEW.organization_id
          AND delivery.target_kind = NEW.sender_kind
          AND delivery.target_id = NEW.sender_id
          AND delivery.delivery_kind = 'message_status'
          AND delivery.message_id = NEW.id
          AND delivery.aggregate_status = NEW.status
          AND delivery.attempt_count = 0
          AND delivery.lease_owner IS NULL
          AND delivery.acked_at IS NULL
          AND delivery.dead_lettered_at IS NULL
    ) THEN
        RAISE EXCEPTION 'message % status transition to % requires an atomic sender delivery',
            NEW.id,
            NEW.status
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER messages_require_status_outbox
AFTER UPDATE ON dm.messages
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_message_status_outbox();

CREATE FUNCTION dm_private.check_system_event_outbox()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM dm.actor_deliveries AS delivery
        WHERE delivery.organization_id = NEW.organization_id
          AND delivery.target_kind = NEW.target_kind
          AND delivery.target_id = NEW.target_silicon_id
          AND delivery.delivery_kind = 'system_event'
          AND delivery.system_event_id = NEW.event_id
          AND delivery.attempt_count = 0
          AND delivery.lease_owner IS NULL
          AND delivery.acked_at IS NULL
          AND delivery.dead_lettered_at IS NULL
    ) THEN
        RAISE EXCEPTION 'system event % must atomically enqueue its pristine target delivery',
            NEW.event_id
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER system_events_require_complete_outbox
AFTER INSERT ON dm.system_events
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.check_system_event_outbox();

CREATE TABLE dm.actor_delivery_ack_cursors (
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    consumer_id text NOT NULL,
    last_acked_sequence bigint NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, actor_kind, actor_id, consumer_id),
    CONSTRAINT actor_delivery_ack_cursors_stream_fk
        FOREIGN KEY (organization_id, actor_kind, actor_id)
        REFERENCES dm.actor_delivery_streams (organization_id, actor_kind, actor_id)
        ON DELETE CASCADE,
    CONSTRAINT actor_delivery_ack_cursors_consumer_id_length
        CHECK (char_length(consumer_id) BETWEEN 1 AND 255),
    CONSTRAINT actor_delivery_ack_cursors_consumer_id_no_control_characters
        CHECK (consumer_id !~ '[[:cntrl:]]'),
    CONSTRAINT actor_delivery_ack_cursors_nonnegative_sequence
        CHECK (last_acked_sequence >= 0)
);

COMMENT ON TABLE dm.actor_delivery_ack_cursors IS
    'Per-device or per-client cumulative resume cursor; server-wide ACK watermark remains on the actor stream.';

CREATE FUNCTION dm_private.enforce_actor_ack_cursor_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    stream_next_sequence bigint;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
            OR NEW.actor_kind IS DISTINCT FROM OLD.actor_kind
            OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
            OR NEW.consumer_id IS DISTINCT FROM OLD.consumer_id
            OR NEW.created_at IS DISTINCT FROM OLD.created_at
        THEN
            RAISE EXCEPTION 'delivery ACK cursor identity is immutable' USING ERRCODE = '23514';
        END IF;

        IF NEW.last_acked_sequence < OLD.last_acked_sequence THEN
            RAISE EXCEPTION 'delivery ACK cursor cannot move backward' USING ERRCODE = '23514';
        END IF;
    END IF;

    SELECT next_sequence
    INTO stream_next_sequence
    FROM dm.actor_delivery_streams
    WHERE organization_id = NEW.organization_id
      AND actor_kind = NEW.actor_kind
      AND actor_id = NEW.actor_id
    FOR UPDATE;

    IF NEW.last_acked_sequence >= stream_next_sequence THEN
        RAISE EXCEPTION 'cannot ACK an actor delivery sequence that has not been allocated'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER actor_delivery_ack_cursors_enforce_policy
BEFORE INSERT OR UPDATE ON dm.actor_delivery_ack_cursors
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_actor_ack_cursor_policy();

CREATE TRIGGER actor_delivery_ack_cursors_set_updated_at
BEFORE UPDATE ON dm.actor_delivery_ack_cursors
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE INDEX actor_delivery_ack_cursors_stale_idx
    ON dm.actor_delivery_ack_cursors (updated_at, organization_id, actor_kind, actor_id);

CREATE TABLE dm.realtime_sessions (
    id uuid PRIMARY KEY,
    instance_id text NOT NULL,
    consumer_id text NOT NULL,
    authenticated_subject text NOT NULL,
    connected_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    last_heartbeat_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    last_pong_at timestamptz,
    lease_expires_at timestamptz NOT NULL,
    disconnected_at timestamptz,
    close_code integer,
    close_reason text,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    CONSTRAINT realtime_sessions_non_nil_id
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT realtime_sessions_instance_id_length
        CHECK (char_length(instance_id) BETWEEN 1 AND 255),
    CONSTRAINT realtime_sessions_consumer_id_length
        CHECK (char_length(consumer_id) BETWEEN 1 AND 255),
    CONSTRAINT realtime_sessions_subject_length
        CHECK (char_length(authenticated_subject) BETWEEN 1 AND 255),
    CONSTRAINT realtime_sessions_lease_order CHECK (lease_expires_at > last_heartbeat_at),
    CONSTRAINT realtime_sessions_pong_order CHECK (
        last_pong_at IS NULL
        OR (last_pong_at >= connected_at AND last_pong_at <= last_heartbeat_at)
    ),
    CONSTRAINT realtime_sessions_disconnect_order CHECK (
        disconnected_at IS NULL OR disconnected_at >= connected_at
    ),
    CONSTRAINT realtime_sessions_close_consistency CHECK (
        (disconnected_at IS NULL AND close_code IS NULL AND close_reason IS NULL)
        OR (disconnected_at IS NOT NULL)
    ),
    CONSTRAINT realtime_sessions_close_code_range
        CHECK (close_code IS NULL OR close_code BETWEEN 1000 AND 4999),
    CONSTRAINT realtime_sessions_close_reason_size
        CHECK (close_reason IS NULL OR octet_length(close_reason) <= 123)
);

COMMENT ON TABLE dm.realtime_sessions IS
    'Cross-instance WebSocket lease without bearer tokens or other credential material.';

CREATE TRIGGER realtime_sessions_set_updated_at
BEFORE UPDATE ON dm.realtime_sessions
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE FUNCTION dm_private.enforce_realtime_session_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.instance_id IS DISTINCT FROM OLD.instance_id
        OR NEW.consumer_id IS DISTINCT FROM OLD.consumer_id
        OR NEW.authenticated_subject IS DISTINCT FROM OLD.authenticated_subject
        OR NEW.connected_at IS DISTINCT FROM OLD.connected_at
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'realtime session identity is immutable' USING ERRCODE = '23514';
    END IF;

    IF NEW.last_heartbeat_at < OLD.last_heartbeat_at
        OR (
            OLD.last_pong_at IS NOT NULL
            AND (
                NEW.last_pong_at IS NULL
                OR NEW.last_pong_at < OLD.last_pong_at
            )
        )
    THEN
        RAISE EXCEPTION 'realtime heartbeat and pong timestamps cannot move backward'
            USING ERRCODE = '23514';
    END IF;

    IF OLD.disconnected_at IS NOT NULL AND (
        NEW.disconnected_at IS DISTINCT FROM OLD.disconnected_at
        OR NEW.close_code IS DISTINCT FROM OLD.close_code
        OR NEW.close_reason IS DISTINCT FROM OLD.close_reason
        OR NEW.last_heartbeat_at IS DISTINCT FROM OLD.last_heartbeat_at
        OR NEW.last_pong_at IS DISTINCT FROM OLD.last_pong_at
        OR NEW.lease_expires_at IS DISTINCT FROM OLD.lease_expires_at
    ) THEN
        RAISE EXCEPTION 'closed realtime sessions are immutable' USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER realtime_sessions_enforce_update_policy
BEFORE UPDATE ON dm.realtime_sessions
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_realtime_session_update_policy();

CREATE INDEX realtime_sessions_active_lease_idx
    ON dm.realtime_sessions (lease_expires_at, instance_id, id)
    WHERE disconnected_at IS NULL;
CREATE INDEX realtime_sessions_consumer_idx
    ON dm.realtime_sessions (consumer_id, connected_at DESC, id DESC);

CREATE TABLE dm.realtime_session_actors (
    session_id uuid NOT NULL,
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    activity dm.presence_activity,
    activity_set_at timestamptz,
    activity_expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (session_id, organization_id, actor_kind, actor_id),
    CONSTRAINT realtime_session_actors_session_fk
        FOREIGN KEY (session_id)
        REFERENCES dm.realtime_sessions (id)
        ON DELETE CASCADE,
    CONSTRAINT realtime_session_actors_actor_fk
        FOREIGN KEY (organization_id, actor_kind, actor_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT,
    CONSTRAINT realtime_session_actors_activity_consistency CHECK (
        (
            activity IS NULL
            AND activity_set_at IS NULL
            AND activity_expires_at IS NULL
        )
        OR (
            activity IS NOT NULL
            AND activity_set_at IS NOT NULL
            AND activity_expires_at IS NOT NULL
            AND activity_expires_at > activity_set_at
        )
    )
);

CREATE TRIGGER realtime_session_actors_set_updated_at
BEFORE UPDATE ON dm.realtime_session_actors
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE INDEX realtime_session_actors_presence_idx
    ON dm.realtime_session_actors (
        organization_id,
        actor_kind,
        actor_id,
        session_id
    );
CREATE INDEX realtime_session_actors_activity_expiry_idx
    ON dm.realtime_session_actors (activity_expires_at, session_id)
    WHERE activity IS NOT NULL;

CREATE TABLE dm.actor_presence_state (
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    last_seen_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (organization_id, actor_kind, actor_id),
    CONSTRAINT actor_presence_state_actor_fk
        FOREIGN KEY (organization_id, actor_kind, actor_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE CASCADE,
    CONSTRAINT actor_presence_state_last_seen_order
        CHECK (last_seen_at IS NULL OR last_seen_at >= created_at)
);

CREATE TRIGGER actor_presence_state_set_updated_at
BEFORE UPDATE ON dm.actor_presence_state
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE FUNCTION dm_private.enforce_actor_presence_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.actor_kind IS DISTINCT FROM OLD.actor_kind
        OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'actor presence identity is immutable' USING ERRCODE = '23514';
    END IF;

    IF OLD.last_seen_at IS NOT NULL
        AND (NEW.last_seen_at IS NULL OR NEW.last_seen_at < OLD.last_seen_at)
    THEN
        RAISE EXCEPTION 'actor last-seen time cannot move backward' USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER actor_presence_state_enforce_update_policy
BEFORE UPDATE ON dm.actor_presence_state
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_actor_presence_update_policy();

CREATE INDEX actor_presence_state_last_seen_idx
    ON dm.actor_presence_state (organization_id, last_seen_at DESC, actor_kind, actor_id);

CREATE TABLE dm.idempotency_records (
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    operation text NOT NULL,
    idempotency_key text NOT NULL,
    request_hash bytea NOT NULL,
    status dm.idempotency_status NOT NULL DEFAULT 'in_progress',
    lease_owner uuid,
    lease_expires_at timestamptz,
    resource_type text,
    resource_id uuid,
    response_status integer,
    response_headers jsonb,
    response_body jsonb,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (
        organization_id,
        actor_kind,
        actor_id,
        operation,
        idempotency_key
    ),
    CONSTRAINT idempotency_records_actor_fk
        FOREIGN KEY (organization_id, actor_kind, actor_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE RESTRICT,
    CONSTRAINT idempotency_records_operation_format
        CHECK (operation ~ '^[a-z][a-z0-9_.-]{2,127}$'),
    CONSTRAINT idempotency_records_key_length
        CHECK (char_length(idempotency_key) BETWEEN 8 AND 255),
    CONSTRAINT idempotency_records_key_no_control_characters
        CHECK (idempotency_key !~ '[[:cntrl:]]'),
    CONSTRAINT idempotency_records_request_hash_length
        CHECK (octet_length(request_hash) = 32),
    CONSTRAINT idempotency_records_resource_consistency CHECK (
        (resource_type IS NULL) = (resource_id IS NULL)
    ),
    CONSTRAINT idempotency_records_resource_type_length
        CHECK (resource_type IS NULL OR char_length(resource_type) BETWEEN 1 AND 64),
    CONSTRAINT idempotency_records_response_status_range
        CHECK (response_status IS NULL OR response_status BETWEEN 200 AND 599),
    CONSTRAINT idempotency_records_response_headers_object CHECK (
        response_headers IS NULL OR jsonb_typeof(response_headers) = 'object'
    ),
    CONSTRAINT idempotency_records_state_consistency CHECK (
        (
            status = 'in_progress'
            AND lease_owner IS NOT NULL
            AND lease_expires_at IS NOT NULL
            AND response_status IS NULL
            AND response_headers IS NULL
            AND response_body IS NULL
        )
        OR (
            status = 'completed'
            AND lease_owner IS NULL
            AND lease_expires_at IS NULL
            AND response_status IS NOT NULL
        )
    ),
    CONSTRAINT idempotency_records_expiry_order CHECK (expires_at > created_at),
    CONSTRAINT idempotency_records_lease_within_retention CHECK (
        lease_expires_at IS NULL OR expires_at > lease_expires_at
    )
);

COMMENT ON TABLE dm.idempotency_records IS
    'Actor-, organization-, and operation-scoped request serialization with replayable responses.';

CREATE FUNCTION dm_private.enforce_idempotency_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.organization_id IS DISTINCT FROM OLD.organization_id
        OR NEW.actor_kind IS DISTINCT FROM OLD.actor_kind
        OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
        OR NEW.operation IS DISTINCT FROM OLD.operation
        OR NEW.idempotency_key IS DISTINCT FROM OLD.idempotency_key
        OR NEW.request_hash IS DISTINCT FROM OLD.request_hash
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'idempotency scope and request hash are immutable'
            USING ERRCODE = '23514';
    END IF;

    IF OLD.status = 'completed' AND NEW.status <> 'completed' THEN
        RAISE EXCEPTION 'completed idempotency records cannot return to in-progress'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.expires_at < OLD.expires_at THEN
        RAISE EXCEPTION 'idempotency retention expiry cannot move backward'
            USING ERRCODE = '23514';
    END IF;

    IF OLD.status = 'completed' AND (
        NEW.resource_type IS DISTINCT FROM OLD.resource_type
        OR NEW.resource_id IS DISTINCT FROM OLD.resource_id
        OR NEW.response_status IS DISTINCT FROM OLD.response_status
        OR NEW.response_headers IS DISTINCT FROM OLD.response_headers
        OR NEW.response_body IS DISTINCT FROM OLD.response_body
    ) THEN
        RAISE EXCEPTION 'completed idempotency responses are immutable'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER idempotency_records_enforce_update_policy
BEFORE UPDATE ON dm.idempotency_records
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_idempotency_update_policy();

CREATE TRIGGER idempotency_records_set_updated_at
BEFORE UPDATE ON dm.idempotency_records
FOR EACH ROW EXECUTE FUNCTION dm_private.set_updated_at();

CREATE INDEX idempotency_records_in_progress_idx
    ON dm.idempotency_records (lease_expires_at, updated_at)
    WHERE status = 'in_progress';
CREATE INDEX idempotency_records_expiry_idx
    ON dm.idempotency_records (expires_at, organization_id, actor_kind, actor_id);

-- Application roles receive explicit grants through deploy/runtime-grants.sql.
-- No unauthenticated database principal may directly inspect DM state or call
-- private trigger helpers.
REVOKE ALL ON SCHEMA dm, dm_private FROM PUBLIC;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA dm_private FROM PUBLIC;
