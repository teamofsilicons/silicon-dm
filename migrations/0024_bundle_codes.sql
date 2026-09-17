-- Stable conversation-local bundle codes; UUIDs remain private storage keys.
ALTER TABLE dm.message_bundles ADD COLUMN sequence bigint;
WITH numbered AS (
    SELECT id, row_number() OVER (PARTITION BY conversation_id ORDER BY created_at, id) AS sequence
    FROM dm.message_bundles
)
UPDATE dm.message_bundles b SET sequence=n.sequence FROM numbered n WHERE n.id=b.id;
ALTER TABLE dm.message_bundles ALTER COLUMN sequence SET NOT NULL;
ALTER TABLE dm.message_bundles ADD CONSTRAINT bundle_sequence_positive CHECK (sequence > 0);
ALTER TABLE dm.message_bundles ADD CONSTRAINT bundle_sequence_unique UNIQUE (conversation_id, sequence);
ALTER TABLE dm.conversations ADD COLUMN next_bundle_sequence bigint NOT NULL DEFAULT 1 CHECK (next_bundle_sequence > 0);
UPDATE dm.conversations c SET next_bundle_sequence = COALESCE((SELECT max(sequence)+1 FROM dm.message_bundles b WHERE b.conversation_id=c.id),1);
CREATE FUNCTION dm_private.assign_bundle_sequence() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE dm.conversations SET next_bundle_sequence=next_bundle_sequence+1
    WHERE id=NEW.conversation_id AND organization_id=NEW.organization_id
    RETURNING next_bundle_sequence-1 INTO NEW.sequence;
    IF NOT FOUND THEN RAISE EXCEPTION 'bundle conversation missing' USING ERRCODE='23503'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER message_bundles_assign_sequence BEFORE INSERT ON dm.message_bundles
FOR EACH ROW EXECUTE FUNCTION dm_private.assign_bundle_sequence();
CREATE FUNCTION dm_private.immutable_bundle_sequence() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.sequence IS DISTINCT FROM OLD.sequence THEN
        RAISE EXCEPTION 'bundle sequence is immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER message_bundles_immutable_sequence BEFORE UPDATE ON dm.message_bundles
FOR EACH ROW EXECUTE FUNCTION dm_private.immutable_bundle_sequence();
