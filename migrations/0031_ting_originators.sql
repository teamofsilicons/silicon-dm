-- Internal proof provenance is separate from the public message and Ting body.
ALTER TABLE dm.ting_handoffs
    ADD COLUMN originator_kind dm.actor_kind,
    ADD COLUMN originator_id text,
    ADD CONSTRAINT ting_handoffs_complete_originator CHECK
        ((originator_kind IS NULL) = (originator_id IS NULL)),
    ADD CONSTRAINT ting_handoffs_originator_fk FOREIGN KEY
        (organization_id,originator_kind,originator_id)
        REFERENCES dm.actor_snapshots(organization_id,actor_kind,actor_id);
COMMENT ON COLUMN dm.ting_handoffs.originator_id IS
    'Immutable actor that caused the event, never its delivery target. NULL requires explicit recovery, not another user credential.';

-- The legacy IAM adapter exposed only the authenticated actor as a sender.
-- Recover those creates/revisions from their author and receipt transitions from
-- their causal receipt. A legacy autonomous failure has no user provenance.
-- Accepted rows may exist during upgrade, so replace the guard transactionally.
DROP TRIGGER ting_handoffs_immutable ON dm.ting_handoffs;
UPDATE dm.ting_handoffs h SET
    originator_kind=CASE WHEN d.delivery_kind='message' THEN m.sender_kind
        WHEN d.aggregate_status IN ('delivered','read') THEN r.recipient_kind END,
    originator_id=CASE WHEN d.delivery_kind='message' THEN m.sender_id
        WHEN d.aggregate_status IN ('delivered','read') THEN r.recipient_id END
FROM dm.actor_deliveries d
JOIN dm.messages m ON m.id=d.message_id AND m.organization_id=d.organization_id
LEFT JOIN dm.message_receipts r ON r.id=d.receipt_id AND r.message_id=d.message_id
    AND r.organization_id=d.organization_id
WHERE h.delivery_id=d.id;

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
             WHEN NEW.aggregate_status IN ('delivered','read') THEN r.recipient_kind END,
        CASE WHEN NEW.delivery_kind='message' THEN COALESCE(origin->>'actor_id',m.sender_id)
             WHEN NEW.aggregate_status IN ('delivered','read') THEN r.recipient_id END
    FROM dm.messages m
    JOIN dm.conversation_addresses a ON a.id=m.conversation_id AND a.organization_id=m.organization_id
    LEFT JOIN dm.message_history h ON h.message_id=m.id
    LEFT JOIN dm.message_receipts r ON r.id=NEW.receipt_id AND r.message_id=m.id
        AND r.organization_id=m.organization_id
    WHERE m.id=NEW.message_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'Ting handoff requires an addressable source message' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION dm_private.guard_ting_handoff() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.delivery_id,NEW.organization_id,NEW.target_kind,NEW.target_id,NEW.conversation_id,
        NEW.public_conversation_id,NEW.message_sequence,NEW.delivery_sequence,NEW.event,NEW.routing_address,
        NEW.created_at,NEW.originator_kind,NEW.originator_id)
        IS DISTINCT FROM
       (OLD.delivery_id,OLD.organization_id,OLD.target_kind,OLD.target_id,OLD.conversation_id,
        OLD.public_conversation_id,OLD.message_sequence,OLD.delivery_sequence,OLD.event,OLD.routing_address,
        OLD.created_at,OLD.originator_kind,OLD.originator_id)
        OR (OLD.request_body IS NOT NULL AND NEW.request_body IS DISTINCT FROM OLD.request_body) THEN
        RAISE EXCEPTION 'Ting handoff reference, originator and prepared request are immutable' USING ERRCODE='23514';
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
