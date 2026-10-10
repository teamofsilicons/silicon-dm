-- DM 0.15 behaviour on top of the Accounts identity tables (0039): no organizations.
-- Nothing is dropped or deleted. Views and functions keep their names and signatures
-- (runtime grants and old SQL callers keep working); their meaning becomes per account.

-- 1. Freeze IAM-era group access. Every account that can reach a group right now through
--    an explicit invitation, the public flag or an IAM tag becomes an accepted member, so
--    nobody gains or loses access when tags and public groups stop existing.
INSERT INTO dm.group_invitations (
    conversation_id, organization_id, actor_kind, actor_id, status, accepted_at
)
SELECT DISTINCT e.conversation_id, e.organization_id, e.actor_kind, e.actor_id,
    'accepted', clock_timestamp()
FROM dm.eligible_group_members e
JOIN dm.actor_snapshots a
  ON a.organization_id = e.organization_id
 AND a.actor_kind = e.actor_kind
 AND a.actor_id = e.actor_id
ON CONFLICT DO NOTHING;

-- 2. Group access is now exactly the accepted memberships of accounts that still exist.
CREATE OR REPLACE VIEW dm.eligible_group_members AS
SELECT i.conversation_id, i.organization_id, i.actor_kind, i.actor_id
FROM dm.group_invitations i
WHERE i.status = 'accepted'
  AND NOT EXISTS (
      SELECT 1 FROM dm.accounts a WHERE a.uuid = i.actor_id AND a.status = 'deleted'
  );

-- A deleted account is no longer a participant of anything (its messages stay).
CREATE OR REPLACE VIEW dm.effective_conversation_participants AS
SELECT p.*
FROM dm.conversation_participants p
JOIN dm.conversations c ON c.id = p.conversation_id AND c.organization_id = p.organization_id
WHERE (NOT c.is_group OR EXISTS (
        SELECT 1 FROM dm.eligible_group_members e
        WHERE e.conversation_id = p.conversation_id
          AND e.organization_id = p.organization_id
          AND e.actor_kind = p.actor_kind
          AND e.actor_id = p.actor_id))
  AND NOT EXISTS (
      SELECT 1 FROM dm.accounts a WHERE a.uuid = p.actor_id AND a.status = 'deleted'
  );

-- Roster anchors for one group (the per-organization variant stays for old callers).
CREATE FUNCTION dm.sync_group_conversation(p_conversation uuid) RETURNS void LANGUAGE sql AS $$
    INSERT INTO dm.conversation_participants (conversation_id, organization_id, actor_kind, actor_id)
    SELECT e.conversation_id, e.organization_id, e.actor_kind, e.actor_id
    FROM dm.eligible_group_members e
    JOIN dm.actor_snapshots a
      ON a.organization_id = e.organization_id
     AND a.actor_kind = e.actor_kind
     AND a.actor_id = e.actor_id
    WHERE e.conversation_id = p_conversation
    ORDER BY e.actor_kind, e.actor_id
    ON CONFLICT DO NOTHING;
$$;
REVOKE ALL ON FUNCTION dm.sync_group_conversation(uuid) FROM PUBLIC;
SELECT dm.sync_group_conversation(g.conversation_id) FROM dm.groups g;

-- 3. One canonical direct conversation per exact participant set (see 0039).
ALTER TABLE dm.conversations DISABLE TRIGGER conversations_set_updated_at;
WITH ranked AS (
    SELECT id,
        row_number() OVER direct_set AS position,
        first_value(id) OVER direct_set AS canonical_id
    FROM dm.conversations
    WHERE NOT is_group AND superseded_by IS NULL
    WINDOW direct_set AS (PARTITION BY participant_set_hash ORDER BY updated_at DESC, id DESC)
)
UPDATE dm.conversations c
SET superseded_by = ranked.canonical_id
FROM ranked
WHERE ranked.id = c.id AND ranked.position > 1;
ALTER TABLE dm.conversations ENABLE TRIGGER conversations_set_updated_at;
CREATE UNIQUE INDEX conversations_canonical_direct_idx
    ON dm.conversations (participant_set_hash)
    WHERE NOT is_group AND superseded_by IS NULL;

-- 4. Public addresses: groups keep their id; a canonical two-party direct conversation is
--    addressed by its participants' CURRENT public ids (uuids render through dm.accounts,
--    unmapped legacy ids as stored); a superseded one is addressed by its UUID only.
CREATE OR REPLACE VIEW dm.conversation_addresses AS
SELECT g.conversation_id AS id, g.organization_id, g.public_id
FROM dm.groups g
UNION ALL
SELECT c.id, c.organization_id,
    CASE WHEN c.superseded_by IS NULL THEN
        string_agg(COALESCE(a.current_id, p.actor_id), '::'
            ORDER BY p.actor_kind::text COLLATE "C", COALESCE(a.current_id, p.actor_id) COLLATE "C")
    ELSE c.id::text END AS public_id
FROM dm.conversations c
JOIN dm.conversation_participants p ON p.conversation_id = c.id AND p.organization_id = c.organization_id
LEFT JOIN dm.accounts a ON a.uuid = p.actor_id AND a.status = 'active'
WHERE NOT EXISTS (SELECT 1 FROM dm.groups g WHERE g.conversation_id = c.id)
GROUP BY c.id, c.organization_id, c.superseded_by
HAVING count(*) = 2;

-- 5. New groups (in the @accounts partition) are `g:{slug}`, globally unique with -2, -3
--    suffixes. Existing `g:{org}:{slug}` ids never change.
CREATE OR REPLACE FUNCTION dm.assign_group_public_id() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE slug text; base text; candidate text; suffix integer;
BEGIN
 IF TG_OP = 'UPDATE' THEN
  IF NEW.public_id IS DISTINCT FROM OLD.public_id OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
     OR NEW.conversation_id IS DISTINCT FROM OLD.conversation_id THEN
   RAISE EXCEPTION 'group identity is immutable' USING ERRCODE = '23514';
  END IF;
 ELSE
  slug := dm.group_slug(NEW.name);
  IF slug = '' THEN
   RAISE EXCEPTION 'group name must contain an ASCII letter or digit' USING ERRCODE = '23514';
  END IF;
  IF NEW.organization_id = '@accounts' THEN
   base := 'g:' || slug;
   -- Serialize allocation of one base so concurrent creations get different suffixes.
   PERFORM pg_advisory_xact_lock(hashtextextended('dm.group_public_id:' || base, 0));
   candidate := base; suffix := 1;
   WHILE EXISTS (SELECT 1 FROM dm.groups WHERE public_id = candidate) LOOP
    suffix := suffix + 1; candidate := base || '-' || suffix;
   END LOOP;
   NEW.public_id := candidate;
  ELSE
   IF NEW.organization_id !~ '^[a-zA-Z0-9][a-zA-Z0-9_-]*$' THEN
    RAISE EXCEPTION 'organization ID cannot form a group address' USING ERRCODE = '23514';
   END IF;
   NEW.public_id := 'g:' || NEW.organization_id || ':' || slug;
  END IF;
 END IF;
 RETURN NEW;
END;
$$;

-- 6. One delivery stream per account. Every delivery takes its sequence from the target's
--    @accounts stream whatever partition the conversation lives in, so /api/v1/sync reads a
--    single contiguous sequence. A stream created for an account that already has legacy
--    deliveries starts above all of them and records that floor.
CREATE OR REPLACE FUNCTION dm_private.assign_actor_delivery_sequence()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    allocated_sequence bigint;
    legacy_high bigint;
BEGIN
    INSERT INTO dm.actor_snapshots (organization_id, actor_kind, actor_id, status, iam_version)
    VALUES ('@accounts', NEW.target_kind, NEW.target_id, 'active', 1)
    ON CONFLICT (organization_id, actor_kind, actor_id) DO NOTHING;

    IF NOT EXISTS (
        SELECT 1 FROM dm.actor_delivery_streams
        WHERE organization_id = '@accounts'
          AND actor_kind = NEW.target_kind
          AND actor_id = NEW.target_id
    ) THEN
        SELECT COALESCE(max(sequence), 0) INTO legacy_high
        FROM dm.actor_deliveries
        WHERE target_kind = NEW.target_kind AND target_id = NEW.target_id;
        INSERT INTO dm.actor_delivery_streams (
            organization_id, actor_kind, actor_id, next_sequence, sync_floor
        )
        VALUES ('@accounts', NEW.target_kind, NEW.target_id, legacy_high + 1, legacy_high)
        ON CONFLICT (organization_id, actor_kind, actor_id) DO NOTHING;
    END IF;

    PERFORM 1
    FROM dm.actor_delivery_streams
    WHERE organization_id = '@accounts'
      AND actor_kind = NEW.target_kind
      AND actor_id = NEW.target_id
    FOR UPDATE;

    IF EXISTS (SELECT 1 FROM dm.actor_deliveries WHERE id = NEW.id) THEN
        RAISE EXCEPTION 'actor delivery ID % already exists', NEW.id
            USING ERRCODE = '23505';
    END IF;

    IF (
        NEW.delivery_kind = 'message'
        AND EXISTS (
            SELECT 1
            FROM dm.actor_deliveries
            WHERE organization_id = NEW.organization_id
              AND target_kind = NEW.target_kind
              AND target_id = NEW.target_id
              AND delivery_kind = 'message'
              AND message_id = NEW.message_id
              AND delivery_revision = NEW.delivery_revision
        )
    ) OR (
        NEW.delivery_kind = 'message_status'
        AND EXISTS (
            SELECT 1
            FROM dm.actor_deliveries
            WHERE organization_id = NEW.organization_id
              AND target_kind = NEW.target_kind
              AND target_id = NEW.target_id
              AND delivery_kind = 'message_status'
              AND message_id = NEW.message_id
              AND aggregate_status = NEW.aggregate_status
        )
    ) OR (
        NEW.delivery_kind = 'system_event'
        AND EXISTS (
            SELECT 1
            FROM dm.actor_deliveries
            WHERE organization_id = NEW.organization_id
              AND target_kind = NEW.target_kind
              AND target_id = NEW.target_id
              AND delivery_kind = 'system_event'
              AND system_event_id = NEW.system_event_id
        )
    ) THEN
        -- Returning NULL from a BEFORE ROW trigger makes idempotent source
        -- re-enqueue a no-op before the actor sequence can be consumed.
        RETURN NULL;
    END IF;

    UPDATE dm.actor_delivery_streams
    SET next_sequence = next_sequence + 1
    WHERE organization_id = '@accounts'
      AND actor_kind = NEW.target_kind
      AND actor_id = NEW.target_id
    RETURNING next_sequence - 1 INTO allocated_sequence;

    IF allocated_sequence IS NULL THEN
        RAISE EXCEPTION 'delivery target actor does not exist' USING ERRCODE = '23503';
    END IF;

    NEW.sequence := allocated_sequence;
    RETURN NEW;
END;
$$;

-- 7. The issuing app of a proof-authenticated message is as immutable as its routing.
CREATE OR REPLACE FUNCTION dm_private.enforce_message_routing_immutable()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.sender_address IS DISTINCT FROM OLD.sender_address
        OR NEW.recipient_address IS DISTINCT FROM OLD.recipient_address
        OR NEW.via_app IS DISTINCT FROM OLD.via_app THEN
        RAISE EXCEPTION 'message routing is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

-- 8. HTTP contract 4: organization-free identity (Silicon Accounts). Version 3 stays
--    readable for the deprecation window the contract lifecycle already enforces.
INSERT INTO dm.contract_versions (family, version) VALUES ('http', 4)
ON CONFLICT DO NOTHING;
UPDATE dm.contract_versions SET status = 'deprecated', deprecated_at = COALESCE(deprecated_at, clock_timestamp())
WHERE family = 'http' AND version = 3 AND status = 'active';
