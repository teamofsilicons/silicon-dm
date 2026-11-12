ALTER TABLE dm.groups ADD COLUMN picture_url text;
ALTER TABLE dm.conversation_events DROP CONSTRAINT conversation_events_change;
ALTER TABLE dm.conversation_events ADD CONSTRAINT conversation_events_change CHECK (change IN (
    'created', 'invited', 'added', 'invitation_accepted', 'invitation_declined',
    'invitation_cancelled', 'left', 'removed', 'renamed', 'description_changed',
    'picture_changed', 'members_changed', 'owners_changed', 'read'
));
