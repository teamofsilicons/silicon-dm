-- Messages keep content history, not public version counters or deletion revisions.
CREATE TABLE dm.message_history (
    message_id uuid PRIMARY KEY REFERENCES dm.messages(id),
    content jsonb,
    history jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(history)='array'),
    updated_at timestamptz,
    deleted_at timestamptz
);
-- A null historical content references the immutable original message and its attachments.
INSERT INTO dm.message_history(message_id,content,history,updated_at,deleted_at)
SELECT m.id,
       (SELECT r.content FROM dm.message_revisions r WHERE r.message_id=m.id AND r.content IS NOT NULL ORDER BY r.version DESC LIMIT 1),
       CASE WHEN EXISTS(SELECT 1 FROM dm.message_revisions r WHERE r.message_id=m.id AND r.content IS NOT NULL)
       THEN jsonb_build_array(jsonb_build_object('content',NULL,'created_at',m.created_at)) ||
          COALESCE((SELECT jsonb_agg(jsonb_build_object('content',r.content,'created_at',r.created_at) ORDER BY r.version)
                    FROM dm.message_revisions r WHERE r.message_id=m.id AND r.content IS NOT NULL
                    AND r.version < (SELECT max(last.version) FROM dm.message_revisions last WHERE last.message_id=m.id AND last.content IS NOT NULL)), '[]'::jsonb)
       ELSE '[]'::jsonb END,
       (SELECT max(r.created_at) FROM dm.message_revisions r WHERE r.message_id=m.id AND r.content IS NOT NULL),
       (SELECT max(r.deleted_at) FROM dm.message_revisions r WHERE r.message_id=m.id)
FROM dm.messages m WHERE EXISTS(SELECT 1 FROM dm.message_revisions r WHERE r.message_id=m.id);
DROP TABLE dm.message_revisions;
DROP FUNCTION dm_private.enforce_message_revision();
DROP FUNCTION dm_private.enforce_revision_outbox();
DROP FUNCTION dm_private.assert_revision_outbox(uuid);
-- A history mutation must commit with a complete durable notification fan-out.
CREATE FUNCTION dm_private.enforce_history_outbox() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE latest_delivery bigint;
BEGIN
    SELECT max(delivery_revision) INTO latest_delivery FROM dm.actor_deliveries
    WHERE message_id=NEW.message_id AND delivery_kind='message';
    IF EXISTS (
        SELECT 1 FROM dm.messages m JOIN dm.effective_conversation_participants p ON p.conversation_id=m.conversation_id
        WHERE m.id=NEW.message_id AND NOT EXISTS (
            SELECT 1 FROM dm.actor_deliveries d WHERE d.message_id=m.id AND d.delivery_kind='message'
            AND d.delivery_revision=latest_delivery AND d.xmin=pg_current_xact_id()::text::xid AND d.target_kind=p.actor_kind AND d.target_id=p.actor_id AND d.organization_id=p.organization_id
        )
    ) THEN RAISE EXCEPTION 'message history requires complete delivery fan-out' USING ERRCODE='23514'; END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER message_history_outbox AFTER INSERT OR UPDATE ON dm.message_history
DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_history_outbox();
CREATE FUNCTION dm_private.guard_message_history() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'message history is immutable' USING ERRCODE='23514'; END IF;
    PERFORM 1 FROM dm.messages WHERE id=NEW.message_id FOR UPDATE;
    IF TG_OP='UPDATE' THEN
        IF NEW.message_id<>OLD.message_id OR OLD.deleted_at IS NOT NULL THEN
            RAISE EXCEPTION 'deleted messages cannot be changed' USING ERRCODE='23514';
        END IF;
        IF NEW.deleted_at IS NOT NULL THEN
            IF NEW.content IS DISTINCT FROM OLD.content OR NEW.history IS DISTINCT FROM OLD.history OR NEW.updated_at IS DISTINCT FROM OLD.updated_at THEN
                RAISE EXCEPTION 'deletion cannot change content history' USING ERRCODE='23514';
            END IF;
        ELSIF jsonb_array_length(NEW.history)<>jsonb_array_length(OLD.history)+1
              OR (NEW.history - (jsonb_array_length(NEW.history)-1)) IS DISTINCT FROM OLD.history
              OR (NEW.history->-1->'content') IS DISTINCT FROM COALESCE(OLD.content,'null'::jsonb) THEN
            RAISE EXCEPTION 'edit must append the prior content' USING ERRCODE='23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER message_history_guard BEFORE INSERT OR UPDATE OR DELETE ON dm.message_history
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_message_history();
REVOKE ALL ON FUNCTION dm_private.guard_message_history() FROM PUBLIC;
REVOKE ALL ON FUNCTION dm_private.enforce_history_outbox() FROM PUBLIC;
