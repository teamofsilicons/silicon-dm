-- The delivery outbox stops growing, and the lookups that scanned it are keyed (DM 0.17).
--
-- 1. message_recipients: who received each message when it was sent (everyone in the
--    conversation then, the sender left out). A message's aggregate status (delivered to
--    everyone, read by everyone) is decided from it. Before, the receipt trigger took the
--    recipients from the message's revision-1 outbox rows: no index on actor_deliveries starts
--    with message_id, so every receipt scanned the whole outbox (seconds for a bulk receipt of
--    a few hundred messages on a large table), and those rows could never be deleted.
--    Backfilled from the same rows, so nothing changes for messages already sent.
-- 2. An index for a message's own outbox rows: edits and speech announcements take the next
--    delivery revision from them, and the history check reads them.
-- 3. The Ting handoff trigger looks the conversation's address up by its key. Joining the
--    address view on the message's conversation kept the key out of the view's aggregate, so
--    every delivery row aggregated the address of every direct conversation in the service.
-- 4. Retention. A delivery at or below its account's sync_floor is outside every sync scan
--    (scans start above the floor, and an older cursor is told to reset), so it may be
--    deleted; the worker raises the floor past events older than the retention window and
--    deletes them with the conversation events below it, and deletes Ting handoffs that were
--    accepted or discarded that long ago. Nothing else reads those rows.
-- 5. delivered_through_sequence: a position at or below which every message from others has
--    a delivered (or read) receipt by the account, so bulk delivered receipts scan only above
--    it, as bulk read receipts scan above read_through_sequence.
-- 6. Adding a participant locks its conversation FOR NO KEY UPDATE, not FOR UPDATE. Every
--    group change adds its members' participant rows (sync_group_conversation), and FOR
--    UPDATE also waited for the foreign-key checks (KEY SHARE) of writers that already hold an
--    account's stream: a receipt or a speech announcement inserting its Ting handoff, which
--    references the conversation. Those writers then waited for the change's conversation
--    events: a deadlock. NO KEY UPDATE still serializes participant changes with each other,
--    with sealing and with sends (all of which update the row).

CREATE TABLE dm.message_recipients (
    message_id uuid NOT NULL REFERENCES dm.messages (id) ON DELETE RESTRICT,
    recipient_kind dm.actor_kind NOT NULL,
    recipient_id text NOT NULL,
    PRIMARY KEY (message_id, recipient_kind, recipient_id)
);

COMMENT ON TABLE dm.message_recipients IS
    'The accounts a message was delivered to when it was sent, sender excluded; its aggregate status counts their receipts.';

INSERT INTO dm.message_recipients (message_id, recipient_kind, recipient_id)
SELECT DISTINCT d.message_id, d.target_kind, d.target_id
FROM dm.actor_deliveries d
JOIN dm.messages m ON m.id = d.message_id AND m.conversation_id = d.conversation_id
WHERE d.delivery_kind = 'message'
  AND d.delivery_revision = 1
  AND (d.target_kind, d.target_id) <> (m.sender_kind, m.sender_id)
ON CONFLICT DO NOTHING;

CREATE INDEX actor_deliveries_message_revision_idx
    ON dm.actor_deliveries (message_id, delivery_revision)
    WHERE delivery_kind = 'message';

CREATE OR REPLACE FUNCTION dm_private.advance_message_aggregate_status()
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
                  AND receipt.recipient_kind = recipient.recipient_kind
                  AND receipt.recipient_id = recipient.recipient_id
                  AND receipt.status IN ('delivered', 'read')
            )),
        count(*) > 0
            AND bool_and(EXISTS (
                SELECT 1
                FROM dm.message_receipts AS receipt
                WHERE receipt.message_id = NEW.message_id
                  AND receipt.recipient_kind = recipient.recipient_kind
                  AND receipt.recipient_id = recipient.recipient_id
                  AND receipt.status = 'read'
            ))
    INTO all_delivered, all_read
    FROM dm.message_recipients AS recipient
    WHERE recipient.message_id = NEW.message_id;

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

CREATE OR REPLACE FUNCTION dm_private.enqueue_ting_handoff()
RETURNS trigger
LANGUAGE plpgsql
AS $$
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
    -- The address is looked up by the delivery's own conversation (a parameter here), which
    -- the planner pushes into both branches of the view; a delivery's conversation is its
    -- message's (actor_deliveries_message_fk).
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
    JOIN dm.conversation_addresses a ON a.id=NEW.conversation_id
        AND a.organization_id=NEW.organization_id
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

CREATE OR REPLACE FUNCTION dm_private.guard_actor_delivery_retention_delete()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    -- Below the account's sync floor no sync scan reaches the row any more; an acknowledged
    -- row past its retention (the retired WebSocket's rule) may go too.
    IF EXISTS (
        SELECT 1 FROM dm.actor_delivery_streams AS stream
        WHERE stream.organization_id = '@accounts'
          AND stream.actor_kind = OLD.target_kind
          AND stream.actor_id = OLD.target_id
          AND OLD.sequence <= stream.sync_floor
    ) OR (
        OLD.acked_at IS NOT NULL
        AND OLD.retain_until IS NOT NULL
        AND OLD.retain_until <= transaction_timestamp()
    ) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'actor delivery can be deleted only at or below its account''s sync floor'
        USING ERRCODE = '23514';
END;
$$;

CREATE INDEX ting_handoffs_finished_idx
    ON dm.ting_handoffs (created_at, delivery_id)
    WHERE accepted_at IS NOT NULL OR discarded_at IS NOT NULL;

-- Ting sends new, edited and deleted messages before receipts: a bulk receipt queues one
-- status handoff per message, and those must not hold up the next message behind them.
CREATE INDEX ting_handoffs_pending_content_idx
    ON dm.ting_handoffs (next_attempt_at, created_at, delivery_id)
    WHERE accepted_at IS NULL AND discarded_at IS NULL
      AND event IN ('message.created', 'message.updated', 'message.deleted');

ALTER TABLE dm.conversation_read_states
    ADD COLUMN delivered_through_sequence bigint NOT NULL DEFAULT 0,
    ADD CONSTRAINT conversation_read_states_delivered_through
        CHECK (delivered_through_sequence >= 0);

CREATE OR REPLACE FUNCTION dm_private.guard_conversation_participant_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    sealed_at timestamptz;
    group_chat boolean;
BEGIN
    SELECT participants_sealed_at, is_group
    INTO sealed_at, group_chat
    FROM dm.conversations
    WHERE id = NEW.conversation_id
      AND organization_id = NEW.organization_id
    FOR NO KEY UPDATE;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'conversation % does not exist in organization %',
            NEW.conversation_id,
            NEW.organization_id
            USING ERRCODE = '23503';
    END IF;

    IF sealed_at IS NOT NULL AND NOT group_chat THEN
        RAISE EXCEPTION 'conversation participants are sealed and immutable'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;
