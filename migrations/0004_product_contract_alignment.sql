-- Align durable messaging with client-owned voice metadata and retire Hook delivery.

ALTER TABLE dm.drafts
    ADD COLUMN voice_transcript text,
    ADD CONSTRAINT drafts_transcript_length
        CHECK (voice_transcript IS NULL OR char_length(voice_transcript) <= 100000000);

COMMENT ON COLUMN dm.drafts.voice_transcript IS
    'Optional client-provided transcript for the draft voice attachment.';

-- Existing hashes use the v1 shape, which did not include voice duration or
-- transcript. New writes use v2; keeping the version explicit lets the send
-- transaction clear a matching legacy draft without rewriting immutable
-- historical message content or weakening idempotency conflict detection.
ALTER TABLE dm.messages
    ADD COLUMN content_hash_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT messages_content_hash_version
        CHECK (content_hash_version IN (1, 2));

ALTER TABLE dm.drafts
    ADD COLUMN content_hash_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT drafts_content_hash_version
        CHECK (content_hash_version IN (1, 2));

COMMENT ON COLUMN dm.messages.content_hash_version IS
    'Canonical content digest version: 1 before client-owned voice metadata, 2 afterward.';
COMMENT ON COLUMN dm.drafts.content_hash_version IS
    'Canonical content digest version used for safe draft clearing.';

CREATE FUNCTION dm_private.guard_message_content_hash_version()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.content_hash_version IS DISTINCT FROM OLD.content_hash_version THEN
        RAISE EXCEPTION 'message content hash version is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER messages_guard_content_hash_version
BEFORE UPDATE ON dm.messages
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_message_content_hash_version();

CREATE FUNCTION dm_private.guard_draft_content_hash_version()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.content_hash_version = 2 AND NEW.content_hash_version <> 2 THEN
        RAISE EXCEPTION 'draft content hash version cannot move backward'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER drafts_guard_content_hash_version
BEFORE UPDATE ON dm.drafts
FOR EACH ROW EXECUTE FUNCTION dm_private.guard_draft_content_hash_version();

CREATE OR REPLACE FUNCTION dm_private.assert_message_content(p_message_id uuid)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    message_row dm.messages%ROWTYPE;
    has_attachment boolean;
    has_voice boolean;
    has_gif boolean;
    attachment_count bigint;
BEGIN
    SELECT *
    INTO message_row
    FROM dm.messages
    WHERE id = p_message_id
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    SELECT
        count(*) > 0,
        count(*) FILTER (WHERE attachment_kind = 'voice') > 0,
        count(*)
    INTO has_attachment, has_voice, attachment_count
    FROM dm.message_attachments
    WHERE message_id = p_message_id;

    SELECT EXISTS (SELECT 1 FROM dm.message_gifs WHERE message_id = p_message_id)
    INTO has_gif;

    IF attachment_count > 100 THEN
        RAISE EXCEPTION 'message % cannot contain more than 100 combined attachments and voice items',
            p_message_id
            USING ERRCODE = '23514';
    END IF;

    IF message_row.text_content IS NULL AND NOT has_attachment AND NOT has_gif THEN
        RAISE EXCEPTION 'message % must contain text, an attachment, voice, or a GIF', p_message_id
            USING ERRCODE = '23514';
    END IF;

    IF NOT has_voice AND message_row.voice_transcript IS NOT NULL THEN
        RAISE EXCEPTION 'message % has a transcript without a voice attachment', p_message_id
            USING ERRCODE = '23514';
    END IF;

    UPDATE dm.messages
    SET content_sealed_at = transaction_timestamp()
    WHERE id = p_message_id
      AND content_sealed_at IS NULL;
END;
$$;

COMMENT ON COLUMN dm.messages.voice_transcript IS
    'Optional client-provided transcript for the message voice attachment.';
COMMENT ON COLUMN dm.messages.transcription_result IS
    'Deprecated legacy provider outcome retained immutably for historical rows; new writes use the not_applicable default.';
COMMENT ON TYPE dm.transcription_result IS
    'Deprecated legacy provider outcome type retained for historical message data.';

CREATE FUNCTION dm_private.enforce_new_message_transcription_result()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.transcription_result <> 'not_applicable' THEN
        RAISE EXCEPTION 'new messages must use the not_applicable transcription result'
            USING
                ERRCODE = '23514',
                CONSTRAINT = 'messages_new_transcription_result_not_applicable';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER messages_enforce_new_transcription_result
BEFORE INSERT ON dm.messages
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_new_message_transcription_result();

ALTER TABLE dm.message_attachments
    DROP CONSTRAINT message_attachments_duration_limit,
    DROP CONSTRAINT message_attachments_duration_kind,
    ADD CONSTRAINT message_attachments_duration_contract CHECK (
        (attachment_kind = 'attachment' AND duration_milliseconds IS NULL)
        OR (
            attachment_kind = 'voice'
            AND duration_milliseconds BETWEEN 1 AND 172800000
        )
    ) NOT VALID,
    ADD CONSTRAINT message_attachments_no_url_credentials CHECK (
        permanent_url !~ '^https://[^/?#]*@'
    ) NOT VALID;

ALTER TABLE dm.draft_attachments
    DROP CONSTRAINT draft_attachments_duration_limit,
    DROP CONSTRAINT draft_attachments_duration_kind,
    ADD CONSTRAINT draft_attachments_duration_contract CHECK (
        (attachment_kind = 'attachment' AND duration_milliseconds IS NULL)
        OR (
            attachment_kind = 'voice'
            AND duration_milliseconds BETWEEN 1 AND 172800000
        )
    ) NOT VALID,
    ADD CONSTRAINT draft_attachments_no_url_credentials CHECK (
        permanent_url !~ '^https://[^/?#]*@'
    ) NOT VALID;

-- NOT VALID deliberately preserves readable historical v1 rows whose
-- provider did not report a duration, as well as any historical URL that
-- cannot be rewritten without mutating sealed content. PostgreSQL still
-- enforces both checks for every new or changed attachment row.

CREATE OR REPLACE FUNCTION dm_private.assert_draft_attachment_limit(
    p_conversation_id uuid,
    p_actor_kind dm.actor_kind,
    p_actor_id text
)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    attachment_count bigint;
    has_voice boolean;
    draft_voice_transcript text;
BEGIN
    SELECT voice_transcript
    INTO draft_voice_transcript
    FROM dm.drafts
    WHERE conversation_id = p_conversation_id
      AND actor_kind = p_actor_kind
      AND actor_id = p_actor_id;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    SELECT
        count(*),
        count(*) FILTER (WHERE attachment_kind = 'voice') > 0
    INTO attachment_count, has_voice
    FROM dm.draft_attachments
    WHERE conversation_id = p_conversation_id
      AND actor_kind = p_actor_kind
      AND actor_id = p_actor_id;

    IF attachment_count > 100 THEN
        RAISE EXCEPTION 'draft cannot contain more than 100 combined attachments and voice items'
            USING ERRCODE = '23514';
    END IF;

    IF NOT has_voice AND draft_voice_transcript IS NOT NULL THEN
        RAISE EXCEPTION 'draft has a transcript without a voice attachment'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

-- Hook has been retired from the product contract. Preserve historical source
-- and terminal rows, but stop retrying every outstanding legacy delivery.
-- SHARE ROW EXCLUSIVE prevents a legacy writer from inserting between the
-- retirement sweep and installation of the forward-only constraint, while
-- continuing to permit readers throughout the cutover.
LOCK TABLE dm.actor_deliveries IN SHARE ROW EXCLUSIVE MODE;

UPDATE dm.actor_deliveries
SET lease_owner = NULL,
    lease_started_at = NULL,
    lease_expires_at = NULL,
    last_error_code = 'delivery_kind_retired',
    dead_lettered_at = GREATEST(created_at, transaction_timestamp())
WHERE delivery_kind = 'system_event'
  AND acked_at IS NULL
  AND dead_lettered_at IS NULL;

ALTER TABLE dm.actor_deliveries
    ADD CONSTRAINT actor_deliveries_no_new_system_events
        CHECK (delivery_kind <> 'system_event') NOT VALID;

REVOKE EXECUTE ON FUNCTION dm_private.guard_message_content_hash_version() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION dm_private.guard_draft_content_hash_version() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION dm_private.enforce_new_message_transcription_result() FROM PUBLIC;
