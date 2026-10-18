-- Activity about one conversation (DM 0.17): what a device is doing (typing, recording a voice
-- message…) is said about the conversation it does it in (renew_presence's conversation_id), and
-- others hear it in that conversation only (GET /api/v1/presence/{account}?conversation=…). Before,
-- an account's activity showed in every conversation shared with it: typing to Bo showed as typing
-- in Cy's thread, and told Cy what Ada was doing elsewhere.
--
-- The column is set and cleared with the activity by DM 0.17. It has no foreign key and no check
-- tying it to `activity` on purpose: DM 0.16 (a rollback) writes these rows without knowing the
-- column, so it may leave a conversation beside a cleared activity, and 0.17 reads the column only
-- while the activity it belongs to is current. A conversation named here was one the account was
-- in when it said so; readers show the activity only to accounts in that conversation, so a stale
-- or dangling value shows nothing to anyone.
ALTER TABLE dm.client_presence_leases ADD COLUMN activity_conversation_id uuid;
COMMENT ON COLUMN dm.client_presence_leases.activity_conversation_id IS
    'The conversation this device''s current activity is about (NULL: no conversation named).';
