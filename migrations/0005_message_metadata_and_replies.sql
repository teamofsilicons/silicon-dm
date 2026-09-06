-- Preserve arbitrary message metadata and same-conversation reply references.
ALTER TABLE dm.messages
    ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}',
    ADD COLUMN reply_to_message_id uuid,
    ADD CONSTRAINT messages_metadata_object CHECK (jsonb_typeof(metadata) = 'object'),
    ADD CONSTRAINT messages_reply_fk FOREIGN KEY (reply_to_message_id, conversation_id, organization_id)
        REFERENCES dm.messages (id, conversation_id, organization_id),
    DROP CONSTRAINT messages_content_hash_version,
    ADD CONSTRAINT messages_content_hash_version CHECK (content_hash_version IN (1, 2, 3));

ALTER TABLE dm.drafts
    ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}',
    ADD COLUMN reply_to_message_id uuid,
    ADD CONSTRAINT drafts_metadata_object CHECK (jsonb_typeof(metadata) = 'object'),
    ADD CONSTRAINT drafts_reply_fk FOREIGN KEY (reply_to_message_id, conversation_id, organization_id)
        REFERENCES dm.messages (id, conversation_id, organization_id),
    DROP CONSTRAINT drafts_content_hash_version,
    ADD CONSTRAINT drafts_content_hash_version CHECK (content_hash_version IN (1, 2, 3));

CREATE OR REPLACE FUNCTION dm_private.guard_draft_content_hash_version()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.content_hash_version < OLD.content_hash_version THEN
        RAISE EXCEPTION 'draft content hash version cannot move backward' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION dm_private.guard_message_metadata()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.metadata IS DISTINCT FROM OLD.metadata
        OR NEW.reply_to_message_id IS DISTINCT FROM OLD.reply_to_message_id THEN
        RAISE EXCEPTION 'original message metadata and reply are immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER messages_guard_metadata BEFORE UPDATE ON dm.messages
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_message_metadata();
REVOKE EXECUTE ON FUNCTION dm_private.guard_message_metadata() FROM PUBLIC;

COMMENT ON COLUMN dm.messages.metadata IS 'Caller-owned JSON object; always present in the public message.';
COMMENT ON COLUMN dm.messages.content_hash_version IS
    '1: legacy voice, 2: voice duration/transcript, 3: metadata/replies. Empty new fields retain the v2 digest.';
