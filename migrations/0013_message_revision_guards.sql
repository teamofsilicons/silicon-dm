-- Preserve every accepted edit and require its complete outbox at commit.
CREATE FUNCTION dm_private.enforce_message_revision()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    previous_version bigint;
    previous_deleted_at timestamptz;
    parent_conversation uuid;
    parent_organization text;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'message revisions are immutable' USING ERRCODE = '23514';
    END IF;

    SELECT conversation_id, organization_id
    INTO parent_conversation, parent_organization
    FROM dm.messages WHERE id = NEW.message_id FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'revision message does not exist' USING ERRCODE = '23503';
    END IF;
    SELECT version, deleted_at INTO previous_version, previous_deleted_at
    FROM dm.message_revisions WHERE message_id = NEW.message_id
    ORDER BY version DESC LIMIT 1;
    IF NEW.version <> COALESCE(previous_version, 1) + 1 OR previous_deleted_at IS NOT NULL THEN
        RAISE EXCEPTION 'revision must advance one version and cannot resurrect a deleted message'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.content IS NOT NULL THEN
        IF jsonb_typeof(NEW.content->'metadata') IS DISTINCT FROM 'object' THEN
            RAISE EXCEPTION 'revision metadata must be an object' USING ERRCODE = '23514';
        END IF;
        IF NEW.content->>'reply_to_message_id' IS NOT NULL AND (
            (NEW.content->>'reply_to_message_id')::uuid = NEW.message_id OR NOT EXISTS (
                SELECT 1 FROM dm.messages
                WHERE id = (NEW.content->>'reply_to_message_id')::uuid
                  AND conversation_id = parent_conversation
                  AND organization_id = parent_organization
            )
        ) THEN
            RAISE EXCEPTION 'revision reply must be another message in the same conversation'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER message_revision_guard BEFORE INSERT OR UPDATE OR DELETE
ON dm.message_revisions FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_message_revision();

CREATE FUNCTION dm_private.assert_revision_outbox(p_revision_id uuid)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM dm.message_revisions AS revision
        JOIN dm.messages AS message ON message.id = revision.message_id
        JOIN dm.conversation_participants AS participant ON participant.conversation_id = message.conversation_id
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

CREATE FUNCTION dm_private.enforce_revision_outbox()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM dm_private.assert_revision_outbox(NEW.id);
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER message_revision_outbox_guard AFTER INSERT
ON dm.message_revisions DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_revision_outbox();

REVOKE ALL ON FUNCTION dm_private.enforce_message_revision() FROM PUBLIC;
REVOKE ALL ON FUNCTION dm_private.assert_revision_outbox(uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION dm_private.enforce_revision_outbox() FROM PUBLIC;
