-- Named groups retain a stable conversation identity and full history.
-- Direct conversations keep their existing sealed participant-set guarantees.
ALTER TABLE dm.conversations ADD COLUMN is_group boolean NOT NULL DEFAULT false;
ALTER TABLE dm.iam_membership_projections ADD COLUMN tag_ids uuid[];
CREATE TABLE dm.groups (
 conversation_id uuid PRIMARY KEY,
 organization_id text NOT NULL,
 name text NOT NULL CHECK (char_length(btrim(name)) BETWEEN 1 AND 120),
 description text NOT NULL DEFAULT '' CHECK (char_length(description) <= 4000),
 is_public boolean NOT NULL DEFAULT false,
 tag_ids uuid[] NOT NULL DEFAULT '{}',
 version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
 FOREIGN KEY (conversation_id,organization_id) REFERENCES dm.conversations(id,organization_id)
);
CREATE INDEX groups_organization ON dm.groups(organization_id,conversation_id);
CREATE TABLE dm.group_invitations (
 conversation_id uuid NOT NULL,
 organization_id text NOT NULL,
 actor_kind dm.actor_kind NOT NULL,
 actor_id text NOT NULL,
 PRIMARY KEY(conversation_id,actor_kind,actor_id),
 FOREIGN KEY(conversation_id) REFERENCES dm.groups(conversation_id),
 FOREIGN KEY(conversation_id,organization_id) REFERENCES dm.conversations(id,organization_id),
 FOREIGN KEY(organization_id,actor_kind,actor_id) REFERENCES dm.actor_snapshots(organization_id,actor_kind,actor_id)
);
-- Roster rows are durable identity/FK anchors. Effective access is evaluated from
-- current IAM projections and group rules; historical rows never grant group access.
CREATE VIEW dm.eligible_group_members AS
 SELECT DISTINCT g.conversation_id,g.organization_id,m.actor_kind,m.actor_id
 FROM dm.groups g
 JOIN dm.iam_membership_projections m ON m.organization_id=g.organization_id AND m.status='active'
 JOIN dm.organization_snapshots o ON o.organization_id=g.organization_id AND o.status='active'
 WHERE EXISTS(SELECT 1 FROM dm.group_invitations i WHERE i.conversation_id=g.conversation_id
   AND i.organization_id=g.organization_id AND i.actor_kind=m.actor_kind AND i.actor_id=m.actor_id)
 OR (g.is_public AND m.actor_kind='carbon')
 OR (NOT g.is_public AND g.tag_ids && COALESCE(m.tag_ids,'{}'::uuid[]));
CREATE VIEW dm.effective_conversation_participants AS
 SELECT p.* FROM dm.conversation_participants p
 JOIN dm.conversations c ON c.id=p.conversation_id AND c.organization_id=p.organization_id
 WHERE NOT c.is_group OR EXISTS(SELECT 1 FROM dm.eligible_group_members e
  WHERE e.conversation_id=p.conversation_id AND e.organization_id=p.organization_id
   AND e.actor_kind=p.actor_kind AND e.actor_id=p.actor_id);
CREATE FUNCTION dm.sync_group_participants(p_org text) RETURNS void LANGUAGE sql AS $$
 INSERT INTO dm.conversation_participants(conversation_id,organization_id,actor_kind,actor_id)
 SELECT e.conversation_id,e.organization_id,e.actor_kind,e.actor_id FROM dm.eligible_group_members e
 JOIN dm.actor_snapshots a ON a.organization_id=e.organization_id AND a.actor_kind=e.actor_kind AND a.actor_id=e.actor_id
 WHERE e.organization_id=p_org ORDER BY e.conversation_id,e.actor_kind,e.actor_id
 ON CONFLICT DO NOTHING;
$$;
REVOKE ALL ON FUNCTION dm.sync_group_participants(text) FROM PUBLIC;
CREATE OR REPLACE FUNCTION dm_private.enforce_conversation_update_policy()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.is_group IS DISTINCT FROM OLD.is_group
        OR NEW.id IS DISTINCT FROM OLD.id
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
    FOR UPDATE;

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
ALTER FUNCTION dm_private.assert_conversation_participant_count(uuid) RENAME TO assert_direct_conversation_participant_count;
CREATE FUNCTION dm_private.assert_conversation_participant_count(p_conversation_id uuid)
RETURNS void LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM dm.conversations WHERE id=p_conversation_id AND is_group) THEN
  IF NOT EXISTS(SELECT 1 FROM dm.groups WHERE conversation_id=p_conversation_id) THEN
   RAISE EXCEPTION 'group conversation requires group metadata' USING ERRCODE='23514';
  END IF;
 ELSE
  PERFORM dm_private.assert_direct_conversation_participant_count(p_conversation_id);
 END IF;
END;
$$;
REVOKE ALL ON FUNCTION dm_private.assert_conversation_participant_count(uuid) FROM PUBLIC;
CREATE OR REPLACE FUNCTION dm_private.assert_message_outbox_complete(p_message_id uuid)
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
        FROM dm.effective_conversation_participants AS participant
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
CREATE OR REPLACE FUNCTION dm_private.assert_revision_outbox(p_revision_id uuid)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM dm.message_revisions AS revision
        JOIN dm.messages AS message ON message.id = revision.message_id
        JOIN dm.effective_conversation_participants AS participant ON participant.conversation_id = message.conversation_id
        WHERE revision.id = p_revision_id
          AND NOT EXISTS (
              SELECT 1 FROM dm.actor_deliveries AS delivery
              WHERE delivery.message_id = revision.message_id
                AND delivery.delivery_kind = 'message'
                AND delivery.delivery_revision = revision.version
                AND delivery.organization_id = participant.organization_id
                AND delivery.target_kind = participant.actor_kind
                AND delivery.target_id = participant.actor_id
          )
    ) THEN
        RAISE EXCEPTION 'message revision requires a durable delivery for every participant'
            USING ERRCODE = '23514';
    END IF;
END;
$$;
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
    FROM (SELECT DISTINCT conversation_id,target_kind AS actor_kind,target_id AS actor_id FROM dm.actor_deliveries WHERE message_id=NEW.message_id AND delivery_kind='message' AND delivery_revision=1) AS participant
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
