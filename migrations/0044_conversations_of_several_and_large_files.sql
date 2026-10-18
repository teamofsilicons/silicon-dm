-- Conversations of several, and attachments of any size (DM 0.17). Additive: older clients
-- keep working unchanged.

-- 1. Every conversation has a public address. A conversation of three or more accounts that
--    is not a group (a "conversation of several") is addressed by its UUID, as a conversation
--    kept for its history is. Before this the view had no row for it, so nothing could be sent
--    into one: the Ting handoff of the first delivery found no address ('Ting handoff requires
--    an addressable source message') and the whole send was refused. Groups and two-party
--    conversations keep exactly the addresses they had.
CREATE OR REPLACE VIEW dm.conversation_addresses AS
SELECT g.conversation_id AS id, g.organization_id, g.public_id
FROM dm.groups g
UNION ALL
SELECT c.id, c.organization_id,
    CASE WHEN c.superseded_by IS NULL AND count(*) = 2 THEN
        string_agg(COALESCE(a.current_id, p.actor_id), '::'
            ORDER BY p.actor_kind::text COLLATE "C", COALESCE(a.current_id, p.actor_id) COLLATE "C")
    ELSE c.id::text END AS public_id
FROM dm.conversations c
JOIN dm.conversation_participants p ON p.conversation_id = c.id AND p.organization_id = c.organization_id
LEFT JOIN dm.accounts a ON a.uuid = p.actor_id AND a.status = 'active'
WHERE NOT EXISTS (SELECT 1 FROM dm.groups g WHERE g.conversation_id = c.id)
GROUP BY c.id, c.organization_id, c.superseded_by;

-- 2. An attachment's declared size may be anything Silicon Briefcase stores: up to 5 TiB per
--    file (the Carbon's decision, 2026-10-18; it was 5 GiB). A 48-hour voice note in an
--    uncompressed format is larger than 5 GiB. DM still never fetches or stores the file.
--    NOT VALID: every existing row met the narrower check, so it meets this one, and skipping
--    the scan keeps this migration's locks (both tables, and the view above, which every
--    message read uses) to an instant however many attachments there are. PostgreSQL still
--    enforces the check for every new or changed row.
ALTER TABLE dm.message_attachments
    DROP CONSTRAINT message_attachments_size_limit,
    ADD CONSTRAINT message_attachments_size_limit CHECK (
        declared_size_bytes IS NULL
        OR declared_size_bytes BETWEEN 0 AND 5497558138880
    ) NOT VALID;
ALTER TABLE dm.draft_attachments
    DROP CONSTRAINT draft_attachments_size_limit,
    ADD CONSTRAINT draft_attachments_size_limit CHECK (
        declared_size_bytes IS NULL
        OR declared_size_bytes BETWEEN 0 AND 5497558138880
    ) NOT VALID;
