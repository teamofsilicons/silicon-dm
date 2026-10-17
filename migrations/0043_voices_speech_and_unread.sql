-- Voices, spoken messages, the speech audio cache, speech usage meters and read positions
-- (DM 0.17). Everything is additive: older clients keep working unchanged.

-- A chosen voice. An account without a row speaks in its default voice, which DM computes
-- from the account uuid and never stores. A stored key that left the catalog falls back to
-- the default when read.
CREATE TABLE dm.account_voices (
    account_uuid text COLLATE "C" PRIMARY KEY,
    voice_key text NOT NULL,
    chosen_by_uuid text COLLATE "C" NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT account_voices_key_format CHECK (voice_key ~ '^[a-z][a-z0-9-]{0,31}$'),
    CONSTRAINT account_voices_account_fk FOREIGN KEY (account_uuid) REFERENCES dm.accounts (uuid),
    CONSTRAINT account_voices_chooser_fk FOREIGN KEY (chosen_by_uuid) REFERENCES dm.accounts (uuid)
);
COMMENT ON TABLE dm.account_voices IS
    'Voices chosen by an account itself or, for a Silicon, by its custodian.';

-- The speech of a message as accepted: {script, voice?, pinned: {key, name, voice_id, model}}.
-- Edits keep theirs in message_history.content like every other content field.
ALTER TABLE dm.messages ADD COLUMN speech jsonb;
ALTER TABLE dm.messages ADD CONSTRAINT messages_speech_shape CHECK (
    speech IS NULL OR (
        jsonb_typeof(speech) = 'object'
        AND jsonb_typeof(speech -> 'script') = 'string'
        AND char_length(speech ->> 'script') BETWEEN 1 AND 5000
        AND jsonb_typeof(speech -> 'pinned') = 'object'
    )
);
COMMENT ON COLUMN dm.messages.speech IS
    'Spoken content with the voice DM pinned at acceptance; voice_transcript holds its words.';

CREATE FUNCTION dm_private.guard_message_speech() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.speech IS DISTINCT FROM OLD.speech THEN
        RAISE EXCEPTION 'message speech is immutable; an edit records its speech in message history'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER messages_speech_immutable BEFORE UPDATE OF speech ON dm.messages
    FOR EACH ROW EXECUTE FUNCTION dm_private.guard_message_speech();
REVOKE ALL ON FUNCTION dm_private.guard_message_speech() FROM PUBLIC;

-- Speech is message content, and its words are the message's transcript.
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

    IF COALESCE(message_row.text_content, '') = '' AND NOT has_attachment AND NOT has_gif
        AND message_row.speech IS NULL THEN
        RAISE EXCEPTION 'message % must contain text, an attachment, voice, speech, or a GIF', p_message_id
            USING ERRCODE = '23514';
    END IF;

    IF NOT has_attachment AND message_row.speech IS NULL AND message_row.voice_transcript IS NOT NULL THEN
        RAISE EXCEPTION 'message % has a transcript without a voice attachment or speech', p_message_id
            USING ERRCODE = '23514';
    END IF;

    IF has_voice AND message_row.speech IS NOT NULL THEN
        RAISE EXCEPTION 'message % carries both a voice note and speech', p_message_id
            USING ERRCODE = '23514';
    END IF;

    UPDATE dm.messages
    SET content_sealed_at = transaction_timestamp()
    WHERE id = p_message_id
      AND content_sealed_at IS NULL;
END;
$$;

-- Rendered speech. A derived cache, never an upload: any row may be evicted (least recently
-- used beyond DM_SPEECH_CACHE_MAX_BYTES, or unused for DM_SPEECH_CACHE_TTL_DAYS) and is
-- rendered again on demand. The key is sha256(model|voice_id|script); the audio is a WAV of
-- 24 kHz mono 16-bit PCM.
CREATE TABLE dm.speech_audio (
    cache_key bytea PRIMARY KEY,
    wav bytea NOT NULL,
    etag text NOT NULL,
    duration_ms integer NOT NULL,
    peaks smallint[] NOT NULL,
    bytes bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_used_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT speech_audio_key_length CHECK (octet_length(cache_key) = 32),
    CONSTRAINT speech_audio_etag_format CHECK (etag ~ '^[0-9a-f]{32}$'),
    CONSTRAINT speech_audio_duration CHECK (duration_ms >= 0),
    CONSTRAINT speech_audio_peaks CHECK (cardinality(peaks) = 64),
    CONSTRAINT speech_audio_bytes CHECK (bytes > 0)
);
-- PCM does not compress; uncompressed out-of-line storage also lets a Range request read only
-- the bytes it needs (substring of an EXTERNAL value fetches just those chunks).
ALTER TABLE dm.speech_audio ALTER COLUMN wav SET STORAGE EXTERNAL;
CREATE INDEX speech_audio_last_used_idx ON dm.speech_audio (last_used_at, cache_key);
COMMENT ON TABLE dm.speech_audio IS
    'Derived cache of rendered speech (WAV), keyed by sha256(model|voice_id|script). Never an upload: rows are evicted by size and age and rendered again on demand.';

-- One durable rendering per spoken message version, queued in the transaction that accepts
-- the message or the edit (so a crash can never lose it), retried with backoff, and announced
-- to the conversation as a message update when it is ready or has failed for good.
CREATE TABLE dm.speech_renders (
    message_id uuid NOT NULL,
    cache_key bytea NOT NULL,
    model text NOT NULL,
    voice_id text NOT NULL,
    script text NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    duration_ms integer,
    peaks smallint[],
    attempts integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    lease_id uuid,
    lease_expires_at timestamptz,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    completed_at timestamptz,
    PRIMARY KEY (message_id, cache_key),
    CONSTRAINT speech_renders_message_fk FOREIGN KEY (message_id) REFERENCES dm.messages (id)
        ON DELETE CASCADE,
    CONSTRAINT speech_renders_key_length CHECK (octet_length(cache_key) = 32),
    CONSTRAINT speech_renders_status CHECK (status IN ('pending', 'ready', 'failed')),
    CONSTRAINT speech_renders_script_length CHECK (char_length(script) BETWEEN 1 AND 5000),
    CONSTRAINT speech_renders_ready_has_audio_facts CHECK (
        (status = 'ready') = (duration_ms IS NOT NULL AND peaks IS NOT NULL)
    ),
    CONSTRAINT speech_renders_peaks CHECK (peaks IS NULL OR cardinality(peaks) = 64),
    CONSTRAINT speech_renders_attempts CHECK (attempts >= 0),
    CONSTRAINT speech_renders_lease CHECK ((lease_id IS NULL) = (lease_expires_at IS NULL)),
    CONSTRAINT speech_renders_completion CHECK ((status = 'pending') = (completed_at IS NULL)),
    CONSTRAINT speech_renders_error_length
        CHECK (last_error IS NULL OR char_length(last_error) BETWEEN 1 AND 255)
);
CREATE INDEX speech_renders_due_idx ON dm.speech_renders (next_attempt_at)
    WHERE status = 'pending';
COMMENT ON TABLE dm.speech_renders IS
    'Durable pre-rendering of spoken messages; status, duration and peaks are what clients see.';

-- Per-account speech usage in fixed windows: renders and transcriptions per minute,
-- characters per UTC day. Rows older than two days are removed by the worker.
CREATE TABLE dm.speech_usage (
    account_uuid text COLLATE "C" NOT NULL,
    meter text NOT NULL,
    window_start timestamptz NOT NULL,
    amount bigint NOT NULL,
    PRIMARY KEY (account_uuid, meter, window_start),
    CONSTRAINT speech_usage_meter CHECK (meter IN ('renders', 'characters', 'transcriptions')),
    CONSTRAINT speech_usage_amount CHECK (amount >= 0)
);
CREATE INDEX speech_usage_window_idx ON dm.speech_usage (window_start);

-- Read positions behind unread counts. Every message at or below read_through_sequence is
-- the account's own, deleted, a bundle member or read by the account, so counting unread
-- messages scans only what lies above it. last_read_sequence is the newest message the
-- account has a read receipt for.
CREATE TABLE dm.conversation_read_states (
    conversation_id uuid NOT NULL REFERENCES dm.conversations (id),
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    read_through_sequence bigint NOT NULL DEFAULT 0,
    last_read_sequence bigint,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (conversation_id, actor_kind, actor_id),
    CONSTRAINT conversation_read_states_through CHECK (read_through_sequence >= 0),
    CONSTRAINT conversation_read_states_last CHECK (last_read_sequence IS NULL OR last_read_sequence > 0)
);
CREATE INDEX conversation_read_states_actor_idx
    ON dm.conversation_read_states (actor_kind, actor_id, conversation_id);
-- Read receipts by account and conversation (last read message, advancing read positions).
CREATE INDEX message_receipts_read_by_account_idx
    ON dm.message_receipts (recipient_kind, recipient_id, conversation_id)
    INCLUDE (message_id)
    WHERE status = 'read';

-- Read positions for every existing participant, from the receipts already recorded.
INSERT INTO dm.conversation_read_states
    (conversation_id, actor_kind, actor_id, read_through_sequence, last_read_sequence)
SELECT p.conversation_id, p.actor_kind, p.actor_id,
    COALESCE(
        (SELECT m.sequence - 1 FROM dm.messages m
          WHERE m.conversation_id = p.conversation_id
            AND (m.sender_kind, m.sender_id) IS DISTINCT FROM (p.actor_kind, p.actor_id)
            AND NOT EXISTS (SELECT 1 FROM dm.message_history h
                WHERE h.message_id = m.id AND h.deleted_at IS NOT NULL)
            AND NOT EXISTS (SELECT 1 FROM dm.message_bundle_items b
                WHERE b.message_id = m.id AND b.role = 'member')
            AND NOT EXISTS (SELECT 1 FROM dm.message_receipts r
                WHERE r.message_id = m.id AND r.recipient_kind = p.actor_kind
                  AND r.recipient_id = p.actor_id AND r.status = 'read')
          ORDER BY m.sequence
          LIMIT 1),
        (SELECT max(m.sequence) FROM dm.messages m WHERE m.conversation_id = p.conversation_id),
        0),
    (SELECT max(m.sequence) FROM dm.message_receipts r
        JOIN dm.messages m ON m.id = r.message_id
      WHERE r.conversation_id = p.conversation_id AND r.recipient_kind = p.actor_kind
        AND r.recipient_id = p.actor_id AND r.status = 'read')
FROM (SELECT DISTINCT conversation_id, actor_kind, actor_id FROM dm.conversation_participants) p
ON CONFLICT DO NOTHING;
