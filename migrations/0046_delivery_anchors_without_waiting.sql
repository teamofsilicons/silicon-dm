-- Fan-out that never waits on another writer's directory refresh (DM 0.17). Additive: the
-- triggers do exactly what they did, without one wait.
--
-- Every delivery and conversation event makes sure its target has an @accounts anchor row in
-- actor_snapshots. They did it with INSERT … ON CONFLICT DO NOTHING, which waits for any open
-- transaction that changed that row, and every send, edit and group change refreshes its
-- actor's row (refresh_directory_in) before it fans out. A writer that fans out without
-- taking the directory first (a bulk receipt, the speech worker, account deletion) could so
-- hold one target's stream lock while it waited on another target's row, whose writer waited
-- on that stream: a deadlock, which PostgreSQL ends by failing one of them (a 500 the client
-- has to retry; it happened to sends racing bulk receipts in a group). An anchor that exists
-- is now only read (a plain read never waits), so the stream rows, which every writer locks in
-- (kind, account) order, are the only locks fan-out waits for. A first delivery to a new
-- account still inserts its anchor, as before.

CREATE OR REPLACE FUNCTION dm_private.assign_actor_delivery_sequence()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    allocated_sequence bigint;
    legacy_high bigint;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM dm.actor_snapshots
        WHERE organization_id = '@accounts'
          AND actor_kind = NEW.target_kind
          AND actor_id = NEW.target_id
    ) THEN
        INSERT INTO dm.actor_snapshots (organization_id, actor_kind, actor_id, status, iam_version)
        VALUES ('@accounts', NEW.target_kind, NEW.target_id, 'active', 1)
        ON CONFLICT (organization_id, actor_kind, actor_id) DO NOTHING;
    END IF;

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

CREATE OR REPLACE FUNCTION dm_private.assign_conversation_event_sequence() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    allocated_sequence bigint;
    legacy_high bigint;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM dm.actor_snapshots
        WHERE organization_id = '@accounts' AND actor_kind = NEW.target_kind
          AND actor_id = NEW.target_id
    ) THEN
        INSERT INTO dm.actor_snapshots (organization_id, actor_kind, actor_id, status, iam_version)
        VALUES ('@accounts', NEW.target_kind, NEW.target_id, 'active', 1)
        ON CONFLICT (organization_id, actor_kind, actor_id) DO NOTHING;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM dm.actor_delivery_streams
        WHERE organization_id = '@accounts' AND actor_kind = NEW.target_kind
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
    UPDATE dm.actor_delivery_streams
    SET next_sequence = next_sequence + 1
    WHERE organization_id = '@accounts' AND actor_kind = NEW.target_kind
      AND actor_id = NEW.target_id
    RETURNING next_sequence - 1 INTO allocated_sequence;
    IF allocated_sequence IS NULL THEN
        RAISE EXCEPTION 'conversation event target has no delivery stream' USING ERRCODE = '23503';
    END IF;
    NEW.sequence := allocated_sequence;
    RETURN NEW;
END;
$$;
