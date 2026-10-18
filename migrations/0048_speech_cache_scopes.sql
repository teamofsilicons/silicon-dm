-- Speech audio is kept per conversation, and a preview per account (DM 0.17).
--
-- The cache key was SHA-256 of model|voice_id|script: one cache for every account. Any
-- signed-in account could then ask POST /api/v1/speech for a guessed script and learn from the
-- answer (x-dm-cache: hit, in milliseconds instead of seconds; or 200 against 429 once over its
-- limit) that a message somewhere, in a conversation it cannot read, said exactly that. A key
-- now starts with its scope: `conversation:<uuid>|` for a message's audio (everyone who can
-- read the message hears the same rendering), `account:<storage key>|` for a script rendered
-- for one account (domain::speech::cache_key). Existing renders move to their conversation's
-- key, and their audio with them; audio no rendering points at (previews) is dropped. The cache
-- is derived data, so nothing is lost: a preview renders again when it is asked for.

CREATE TEMPORARY TABLE speech_rekey ON COMMIT DROP AS
SELECT r.message_id, r.cache_key AS old_key,
       sha256(convert_to('conversation:' || m.conversation_id::text || '|' || r.model || '|'
                         || r.voice_id || '|' || r.script, 'UTF8')) AS new_key
FROM dm.speech_renders r
JOIN dm.messages m ON m.id = r.message_id;

INSERT INTO dm.speech_audio (cache_key, wav, etag, duration_ms, peaks, bytes, created_at,
                             last_used_at)
SELECT DISTINCT ON (k.new_key) k.new_key, a.wav, a.etag, a.duration_ms, a.peaks, a.bytes,
       a.created_at, a.last_used_at
FROM speech_rekey k
JOIN dm.speech_audio a ON a.cache_key = k.old_key
ORDER BY k.new_key
ON CONFLICT (cache_key) DO NOTHING;

DELETE FROM dm.speech_audio a
WHERE NOT EXISTS (SELECT 1 FROM speech_rekey k WHERE k.new_key = a.cache_key);

UPDATE dm.speech_renders r
SET cache_key = k.new_key
FROM speech_rekey k
WHERE k.message_id = r.message_id AND k.old_key = r.cache_key;

COMMENT ON TABLE dm.speech_audio IS
    'Derived cache of rendered speech (WAV), keyed by sha256(<scope>|model|voice_id|script) where the scope is the conversation of a message''s audio or the account a preview was rendered for. Never an upload: rows are evicted by size and age and rendered again on demand.';
