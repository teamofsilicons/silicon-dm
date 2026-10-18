-- Conversation events in the sync feed (DM 0.17): when a conversation appears for, changes
-- for or leaves an account (created with it in it, invited, added, an invitation answered or
-- withdrawn, renamed or described, members or owners changed, left, removed). Clients ask for
-- them (GET /api/v1/sync?include=conversations), so an older client never meets an event that
-- names no message. Additive: nothing existing changes.
--
-- Each event takes the next position in the account's own stream (actor_delivery_streams,
-- the one message events use), so /api/v1/sync reads one gapless sequence across both tables
-- and an account's events keep the order their transactions committed in (the stream row is
-- locked until commit). Events only name a conversation and what happened; a client reads
-- the conversation itself with its own permissions.
CREATE TABLE dm.conversation_events (
    id uuid PRIMARY KEY,
    target_kind dm.actor_kind NOT NULL,
    target_id text NOT NULL,
    sequence bigint NOT NULL,
    conversation_id uuid NOT NULL REFERENCES dm.conversations (id),
    change text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT conversation_events_stream_position UNIQUE (target_kind, target_id, sequence),
    CONSTRAINT conversation_events_positive_sequence CHECK (sequence > 0),
    CONSTRAINT conversation_events_change CHECK (change IN (
        'created', 'invited', 'added', 'invitation_accepted', 'invitation_declined',
        'invitation_cancelled', 'left', 'removed', 'renamed', 'description_changed',
        'members_changed', 'owners_changed'
    ))
);
COMMENT ON TABLE dm.conversation_events IS
    'Per-account references to conversation changes, positioned in the account''s sync stream.';

-- The same allocation the actor deliveries make (0040): the account's @accounts stream,
-- created above every legacy delivery when it does not exist yet.
CREATE FUNCTION dm_private.assign_conversation_event_sequence() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    allocated_sequence bigint;
    legacy_high bigint;
BEGIN
    INSERT INTO dm.actor_snapshots (organization_id, actor_kind, actor_id, status, iam_version)
    VALUES ('@accounts', NEW.target_kind, NEW.target_id, 'active', 1)
    ON CONFLICT (organization_id, actor_kind, actor_id) DO NOTHING;
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
CREATE TRIGGER conversation_events_assign_sequence BEFORE INSERT ON dm.conversation_events
    FOR EACH ROW EXECUTE FUNCTION dm_private.assign_conversation_event_sequence();

CREATE FUNCTION dm_private.guard_conversation_event() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'conversation events are immutable' USING ERRCODE = '23514';
END;
$$;
CREATE TRIGGER conversation_events_immutable BEFORE UPDATE ON dm.conversation_events
    FOR EACH ROW EXECUTE FUNCTION dm_private.guard_conversation_event();
REVOKE ALL ON FUNCTION dm_private.assign_conversation_event_sequence() FROM PUBLIC;
REVOKE ALL ON FUNCTION dm_private.guard_conversation_event() FROM PUBLIC;
