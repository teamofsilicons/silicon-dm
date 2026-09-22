-- Ting acceptance is a separate durable handoff, never a DM receipt or socket ACK.
-- Keep the existing actor stream and message schema intact for HTTP synchronization.
CREATE TABLE dm.ting_handoffs (
    delivery_id uuid PRIMARY KEY,
    organization_id text NOT NULL,
    target_kind dm.actor_kind NOT NULL,
    target_id text NOT NULL,
    conversation_id uuid NOT NULL,
    public_conversation_id text NOT NULL,
    message_sequence bigint NOT NULL CHECK (message_sequence > 0),
    delivery_sequence bigint NOT NULL CHECK (delivery_sequence > 0),
    event text NOT NULL CHECK (event IN ('message.created','message.updated','message.deleted',
        'message.delivered','message.read','message.failed')),
    routing_address text,
    created_at timestamptz NOT NULL,
    request_body text CHECK (request_body IS NULL OR
        (octet_length(request_body) <= 262144 AND jsonb_typeof(request_body::jsonb) = 'object')),
    attempt_count bigint NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_error_code text CHECK (last_error_code IS NULL OR char_length(last_error_code) BETWEEN 1 AND 255),
    lease_id uuid,
    lease_owner text,
    lease_expires_at timestamptz,
    accepted_at timestamptz,
    ting_id text,
    ting_created_at timestamptz,
    silent boolean,
    FOREIGN KEY (organization_id,target_kind,target_id)
        REFERENCES dm.actor_snapshots(organization_id,actor_kind,actor_id),
    FOREIGN KEY (conversation_id,organization_id)
        REFERENCES dm.conversations(id,organization_id),
    CHECK ((lease_id IS NULL AND lease_owner IS NULL AND lease_expires_at IS NULL)
        OR (lease_id IS NOT NULL AND char_length(lease_owner) BETWEEN 1 AND 255 AND lease_expires_at IS NOT NULL)),
    CHECK ((accepted_at IS NULL AND ting_id IS NULL AND ting_created_at IS NULL AND silent IS NULL)
        OR (accepted_at IS NOT NULL AND char_length(ting_id) BETWEEN 1 AND 255 AND ting_created_at IS NOT NULL
            AND silent IS NOT NULL AND request_body IS NOT NULL AND lease_id IS NULL))
);
-- No FK to actor_deliveries: its old socket-ACK retention must not erase a pending handoff.
COMMENT ON TABLE dm.ting_handoffs IS
    'Immutable per-recipient references and exact Ting request bytes; acceptance does not change DM message receipts.';
CREATE INDEX ting_handoffs_due ON dm.ting_handoffs(next_attempt_at,created_at,delivery_id)
    WHERE accepted_at IS NULL;

CREATE FUNCTION dm_private.enqueue_ting_handoff() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.delivery_kind NOT IN ('message','message_status') THEN RETURN NEW; END IF;
    INSERT INTO dm.ting_handoffs(delivery_id,organization_id,target_kind,target_id,conversation_id,
        public_conversation_id,message_sequence,delivery_sequence,event,routing_address,created_at)
    SELECT NEW.id,NEW.organization_id,NEW.target_kind,NEW.target_id,NEW.conversation_id,
        a.public_id,m.sequence,NEW.sequence,
        CASE WHEN NEW.delivery_kind='message_status' THEN 'message.' || NEW.aggregate_status::text
             WHEN NEW.delivery_revision=1 THEN 'message.created'
             WHEN h.deleted_at IS NOT NULL THEN 'message.deleted'
             ELSE 'message.updated' END,
        CASE WHEN m.sender_kind=NEW.target_kind AND m.sender_id=NEW.target_id THEN m.sender_address
             ELSE m.recipient_address END,
        NEW.created_at
    FROM dm.messages m
    JOIN dm.conversation_addresses a ON a.id=m.conversation_id AND a.organization_id=m.organization_id
    LEFT JOIN dm.message_history h ON h.message_id=m.id
    WHERE m.id=NEW.message_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'Ting handoff requires an addressable source message' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER actor_deliveries_ting_handoff AFTER INSERT ON dm.actor_deliveries
    FOR EACH ROW EXECUTE FUNCTION dm_private.enqueue_ting_handoff();

-- Preserve every old pending event, including offline recipients. Public message codes
-- are serialized from the immutable sequence by the same Rust protocol helper as HTTP.
INSERT INTO dm.ting_handoffs(delivery_id,organization_id,target_kind,target_id,conversation_id,
    public_conversation_id,message_sequence,delivery_sequence,event,routing_address,created_at)
SELECT d.id,d.organization_id,d.target_kind,d.target_id,d.conversation_id,
    a.public_id,m.sequence,d.sequence,
    CASE WHEN d.delivery_kind='message_status' THEN 'message.' || d.aggregate_status::text
         WHEN d.delivery_revision=1 THEN 'message.created'
         WHEN h.deleted_at IS NOT NULL AND d.delivery_revision=(SELECT max(last.delivery_revision)
             FROM dm.actor_deliveries last WHERE last.message_id=d.message_id AND last.delivery_kind='message')
             THEN 'message.deleted'
         ELSE 'message.updated' END,
    CASE WHEN m.sender_kind=d.target_kind AND m.sender_id=d.target_id THEN m.sender_address
         ELSE m.recipient_address END,
    d.created_at
FROM dm.actor_deliveries d
JOIN dm.messages m ON m.id=d.message_id
JOIN dm.conversation_addresses a ON a.id=d.conversation_id AND a.organization_id=d.organization_id
LEFT JOIN dm.message_history h ON h.message_id=m.id
WHERE d.acked_at IS NULL AND d.dead_lettered_at IS NULL AND d.delivery_kind IN ('message','message_status');

CREATE FUNCTION dm_private.guard_ting_handoff() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.delivery_id,NEW.organization_id,NEW.target_kind,NEW.target_id,NEW.conversation_id,
        NEW.public_conversation_id,NEW.message_sequence,NEW.delivery_sequence,NEW.event,NEW.routing_address,NEW.created_at)
        IS DISTINCT FROM
       (OLD.delivery_id,OLD.organization_id,OLD.target_kind,OLD.target_id,OLD.conversation_id,
        OLD.public_conversation_id,OLD.message_sequence,OLD.delivery_sequence,OLD.event,OLD.routing_address,OLD.created_at)
        OR (OLD.request_body IS NOT NULL AND NEW.request_body IS DISTINCT FROM OLD.request_body) THEN
        RAISE EXCEPTION 'Ting handoff reference and prepared request are immutable' USING ERRCODE='23514';
    END IF;
    IF OLD.accepted_at IS NOT NULL AND NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'accepted Ting handoff is immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER ting_handoffs_immutable BEFORE UPDATE ON dm.ting_handoffs
    FOR EACH ROW EXECUTE FUNCTION dm_private.guard_ting_handoff();
REVOKE ALL ON FUNCTION dm_private.enqueue_ting_handoff() FROM PUBLIC;
REVOKE ALL ON FUNCTION dm_private.guard_ting_handoff() FROM PUBLIC;
