-- Public addresses are derived from immutable direct participant sets. UUID keys
-- and the atomic conversation message counter remain unchanged, including history.
CREATE VIEW dm.conversation_addresses AS
SELECT g.conversation_id AS id, g.organization_id, g.public_id
FROM dm.groups g
UNION ALL
SELECT c.id, c.organization_id,
       string_agg(p.actor_id, '::' ORDER BY p.actor_kind::text COLLATE "C", p.actor_id COLLATE "C") AS public_id
FROM dm.conversations c
JOIN dm.conversation_participants p ON p.conversation_id=c.id AND p.organization_id=c.organization_id
WHERE NOT EXISTS (SELECT 1 FROM dm.groups g WHERE g.conversation_id=c.id)
GROUP BY c.id,c.organization_id
HAVING count(*)=2;

INSERT INTO dm.contract_versions(family,version) VALUES('http',2),('websocket',4);
UPDATE dm.contract_versions SET status='deprecated',deprecated_at=clock_timestamp()
WHERE (family='http' AND version=1) OR (family='websocket' AND version=3);

-- Audio is now an ordinary attachment URL. The transcript needs an attachment,
-- without requiring the retired voice object or fetching media to infer its type.
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

    IF COALESCE(message_row.text_content, '') = '' AND NOT has_attachment AND NOT has_gif THEN
        RAISE EXCEPTION 'message % must contain text, an attachment, voice, or a GIF', p_message_id
            USING ERRCODE = '23514';
    END IF;

    IF NOT has_attachment AND message_row.voice_transcript IS NOT NULL THEN
        RAISE EXCEPTION 'message % has a transcript without a voice attachment', p_message_id
            USING ERRCODE = '23514';
    END IF;

    UPDATE dm.messages
    SET content_sealed_at = transaction_timestamp()
    WHERE id = p_message_id
      AND content_sealed_at IS NULL;
END;
$$;

