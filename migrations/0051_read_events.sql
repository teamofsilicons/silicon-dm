-- Read events (DM 0.17): when an account reads (a read receipt, one by one or in bulk), its own
-- sync feed gets a conversation event `read` with the conversation's unread facts afterwards, so
-- its other devices update their unread badges at once instead of at their next refresh.
--
-- The facts are the ones of the moment the event takes its place in the account's stream: the
-- writer locks the account's stream row first, then counts, so every message delivered before
-- the event is counted and none delivered after it, and a client applying its feed in order
-- ends with the right count. Like the other conversation events these rows are only read by
-- DM 0.17 (DM 0.16 never reads conversation_events), so 0.16 still runs on the migrated database.
ALTER TABLE dm.conversation_events
    ADD COLUMN unread_count integer,
    ADD COLUMN last_read_sequence bigint;
COMMENT ON COLUMN dm.conversation_events.unread_count IS
    'Read events only: the account''s unread messages in the conversation after it read (at most 1,000).';
COMMENT ON COLUMN dm.conversation_events.last_read_sequence IS
    'Read events only: the sequence of the newest message the account has read in the conversation.';

ALTER TABLE dm.conversation_events DROP CONSTRAINT conversation_events_change;
ALTER TABLE dm.conversation_events ADD CONSTRAINT conversation_events_change CHECK (change IN (
    'created', 'invited', 'added', 'invitation_accepted', 'invitation_declined',
    'invitation_cancelled', 'left', 'removed', 'renamed', 'description_changed',
    'members_changed', 'owners_changed', 'read'
));
ALTER TABLE dm.conversation_events ADD CONSTRAINT conversation_events_read_facts CHECK (
    (change = 'read') = (unread_count IS NOT NULL)
    AND (unread_count IS NULL OR unread_count >= 0)
    AND (last_read_sequence IS NULL OR (change = 'read' AND last_read_sequence > 0))
);
