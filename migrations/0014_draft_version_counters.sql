-- A deleted draft must not release its optimistic concurrency token for reuse.
-- Drain existing writers before backfill and hold the lock until the trigger
-- is installed, so online upgrades cannot leave counters behind current drafts.
LOCK TABLE dm.drafts IN SHARE ROW EXCLUSIVE MODE;

CREATE TABLE dm.draft_version_counters (
    conversation_id uuid NOT NULL,
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    PRIMARY KEY (conversation_id, actor_kind, actor_id),
    FOREIGN KEY (conversation_id, organization_id, actor_kind, actor_id)
        REFERENCES dm.conversation_participants(conversation_id, organization_id, actor_kind, actor_id)
);

INSERT INTO dm.draft_version_counters(conversation_id, organization_id, actor_kind, actor_id, version)
SELECT conversation_id, organization_id, actor_kind, actor_id, version FROM dm.drafts;

CREATE FUNCTION dm_private.allocate_draft_version()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO dm.draft_version_counters AS counter
            (conversation_id, organization_id, actor_kind, actor_id, version)
        VALUES (NEW.conversation_id, NEW.organization_id, NEW.actor_kind, NEW.actor_id, 1)
        ON CONFLICT (conversation_id, actor_kind, actor_id) DO UPDATE
        SET version = counter.version + 1
        RETURNING version INTO NEW.version;
    ELSE
        -- The existing update guard separately requires exactly OLD.version + 1.
        UPDATE dm.draft_version_counters SET version = NEW.version
        WHERE conversation_id = OLD.conversation_id
          AND organization_id = OLD.organization_id
          AND actor_kind = OLD.actor_kind AND actor_id = OLD.actor_id
          AND version = OLD.version;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'draft version counter is inconsistent' USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER drafts_allocate_version BEFORE INSERT OR UPDATE ON dm.drafts
FOR EACH ROW EXECUTE FUNCTION dm_private.allocate_draft_version();
REVOKE ALL ON FUNCTION dm_private.allocate_draft_version() FROM PUBLIC;

COMMENT ON TABLE dm.draft_version_counters IS
    'Per-participant high-water mark retained across explicit deletion and automatic send clearing; tokens are never reused.';
