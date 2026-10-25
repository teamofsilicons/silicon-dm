-- Delivered now means DM completed the outbound handoff to Ting for every
-- recipient. Read remains an explicit recipient read acknowledgment.
-- The sender update must retain the original send's immutable authority.
ALTER TABLE dm.message_recipients ADD COLUMN outbound_accepted_at timestamptz;
COMMENT ON COLUMN dm.message_recipients.outbound_accepted_at IS
    'Durable evidence that Ting accepted the original outbound handoff; survives transport retention and is not a device receipt.';

CREATE INDEX ting_handoffs_original_message ON dm.ting_handoffs
    (organization_id,conversation_id,message_sequence,target_kind,target_id)
    WHERE event='message.created';

CREATE OR REPLACE FUNCTION dm_private.enqueue_ting_handoff() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    origin jsonb;
BEGIN
    IF NEW.delivery_kind NOT IN ('message','message_status') THEN RETURN NEW; END IF;
    -- Only trusted application transaction code sets this local context. It must
    -- never come from a message payload or a connection/session-wide SET.
    origin := NULLIF(current_setting('dm.ting_originator',true),'')::jsonb;
    IF origin IS NOT NULL AND (
        jsonb_typeof(origin) IS DISTINCT FROM 'object'
        OR origin->>'organization_id' IS DISTINCT FROM NEW.organization_id
        OR origin->>'actor_kind' IS NULL OR origin->>'actor_id' IS NULL
    ) THEN
        RAISE EXCEPTION 'Ting originator must match the source organization' USING ERRCODE='23514';
    END IF;
    INSERT INTO dm.ting_handoffs(delivery_id,organization_id,target_kind,target_id,conversation_id,
        public_conversation_id,message_sequence,delivery_sequence,event,routing_address,created_at,
        originator_kind,originator_id)
    SELECT NEW.id,NEW.organization_id,NEW.target_kind,NEW.target_id,NEW.conversation_id,
        a.public_id,m.sequence,NEW.sequence,
        CASE WHEN NEW.delivery_kind='message_status' THEN 'message.' || NEW.aggregate_status::text
             WHEN NEW.delivery_revision=1 THEN 'message.created'
             WHEN h.deleted_at IS NOT NULL THEN 'message.deleted'
             ELSE 'message.updated' END,
        CASE WHEN m.sender_kind=NEW.target_kind AND m.sender_id=NEW.target_id THEN m.sender_address
             ELSE m.recipient_address END,
        NEW.created_at,
        CASE WHEN NEW.delivery_kind='message' THEN
                COALESCE((origin->>'actor_kind')::dm.actor_kind,m.sender_kind)
             WHEN NEW.aggregate_status = 'delivered' AND NEW.receipt_id IS NULL THEN source.originator_kind
             WHEN NEW.aggregate_status IN ('delivered','read') THEN r.recipient_kind END,
        CASE WHEN NEW.delivery_kind='message' THEN COALESCE(origin->>'actor_id',m.sender_id)
             WHEN NEW.aggregate_status = 'delivered' AND NEW.receipt_id IS NULL THEN source.originator_id
             WHEN NEW.aggregate_status IN ('delivered','read') THEN r.recipient_id END
    FROM dm.messages m
    JOIN dm.conversation_addresses a ON a.id=NEW.conversation_id
        AND a.organization_id=NEW.organization_id
    LEFT JOIN dm.message_history h ON h.message_id=m.id
    LEFT JOIN dm.message_receipts r ON r.id=NEW.receipt_id AND r.message_id=m.id
        AND r.organization_id=m.organization_id
    LEFT JOIN LATERAL (
        -- A server delivery update continues the original authenticated send.
        -- Never borrow the recipient's authority or invent a device receipt.
        SELECT initial.originator_kind,initial.originator_id
        FROM dm.ting_handoffs initial
        WHERE initial.organization_id=m.organization_id
          AND initial.conversation_id=m.conversation_id
          AND initial.message_sequence=m.sequence
          AND initial.event='message.created'
          AND initial.originator_id IS NOT NULL
        ORDER BY initial.created_at,initial.delivery_id
        LIMIT 1
    ) source ON NEW.aggregate_status='delivered' AND NEW.receipt_id IS NULL
    WHERE m.id=NEW.message_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'Ting handoff requires an addressable source message' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;


CREATE FUNCTION dm_private.advance_message_on_ting_acceptance()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    message record;
    all_accepted boolean;
BEGIN
    IF NEW.event <> 'message.created' OR NEW.accepted_at IS NULL
        OR OLD.accepted_at IS NOT NULL THEN
        RETURN NEW;
    END IF;

    -- Serialize with receipts and concurrent recipient acceptances. The next
    -- statement sees handoffs committed by the previous holder of this lock.
    SELECT id,organization_id,conversation_id,sequence,sender_kind,sender_id,status
    INTO message FROM dm.messages
    WHERE organization_id=NEW.organization_id
      AND conversation_id=NEW.conversation_id AND sequence=NEW.message_sequence
    FOR UPDATE;
    IF NOT FOUND OR message.status NOT IN ('waiting','sent') THEN
        RETURN NEW;
    END IF;

    -- Preserve the evidence beyond Ting handoff retention. Sender copies have no
    -- recipient row and cannot advance the aggregate.
    UPDATE dm.message_recipients
    SET outbound_accepted_at=COALESCE(outbound_accepted_at,NEW.accepted_at)
    WHERE message_id=message.id AND recipient_kind=NEW.target_kind AND recipient_id=NEW.target_id;

    SELECT count(*) > 0 AND bool_and(outbound_accepted_at IS NOT NULL)
    INTO all_accepted FROM dm.message_recipients WHERE message_id=message.id;

    IF all_accepted THEN
        UPDATE dm.messages SET status='delivered',
            delivered_at=COALESCE(delivered_at,clock_timestamp())
        WHERE id=message.id;
        -- Atomic sender notification, with no fabricated recipient receipt.
        -- Existing Ting insertion triggers wake the worker after commit.
        INSERT INTO dm.actor_deliveries(id,organization_id,target_kind,target_id,
            delivery_kind,conversation_id,message_id,aggregate_status)
        VALUES(gen_random_uuid(),message.organization_id,message.sender_kind,message.sender_id,
            'message_status',message.conversation_id,message.id,'delivered');
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER ting_handoffs_message_delivered AFTER UPDATE OF accepted_at ON dm.ting_handoffs
    FOR EACH ROW EXECUTE FUNCTION dm_private.advance_message_on_ting_acceptance();
REVOKE ALL ON FUNCTION dm_private.advance_message_on_ting_acceptance() FROM PUBLIC;
COMMENT ON TABLE dm.ting_handoffs IS
    'Immutable per-recipient references and exact Ting request bytes; original recipient acceptance advances Delivered without fabricating device receipts. Read requires an explicit DM receipt.';

-- Carry forward only proven original handoffs still retained at upgrade. No
-- discarded/pending historical notification is replayed or marked accepted.
UPDATE dm.message_recipients r SET outbound_accepted_at=h.accepted_at
FROM dm.messages m,dm.ting_handoffs h
WHERE r.message_id=m.id AND h.organization_id=m.organization_id
  AND h.conversation_id=m.conversation_id AND h.message_sequence=m.sequence
  AND h.event='message.created' AND h.target_kind=r.recipient_kind
  AND h.target_id=r.recipient_id AND h.accepted_at IS NOT NULL;

-- Existing Sent messages with complete outbound evidence get the same visible
-- status as new ones, with one atomic sender notification and no read receipt.
WITH promoted AS (
    UPDATE dm.messages m SET status='delivered',delivered_at=COALESCE(m.delivered_at,
        (SELECT max(outbound_accepted_at) FROM dm.message_recipients WHERE message_id=m.id))
    WHERE m.status IN ('waiting','sent')
      AND EXISTS(SELECT 1 FROM dm.message_recipients WHERE message_id=m.id)
      AND NOT EXISTS(SELECT 1 FROM dm.message_recipients WHERE message_id=m.id AND outbound_accepted_at IS NULL)
    RETURNING m.id,m.organization_id,m.conversation_id,m.sender_kind,m.sender_id
)
INSERT INTO dm.actor_deliveries(id,organization_id,target_kind,target_id,
    delivery_kind,conversation_id,message_id,aggregate_status)
SELECT gen_random_uuid(),organization_id,sender_kind,sender_id,
    'message_status',conversation_id,id,'delivered' FROM promoted
ORDER BY sender_kind,sender_id,id;
