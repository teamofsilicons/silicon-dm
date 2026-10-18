-- A message's recipients are recorded by the database as its outbox rows are written (DM 0.17).
--
-- 0047 made a message's aggregate status (delivered to everyone, read by everyone) count the
-- receipts of the accounts in dm.message_recipients, which the 0.17 service wrote when it sent.
-- A service that does not know the table (0.16, if the code is rolled back after these
-- migrations) would send messages without recipients, and their ticks would never move. The
-- rows now come from the send's own revision-1 outbox rows, the sender left out: the rule the
-- receipt trigger used before 0047 and the backfill in 0047 followed, so every version of the
-- service records them the same way.

CREATE OR REPLACE FUNCTION dm_private.record_message_recipient()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO dm.message_recipients (message_id, recipient_kind, recipient_id)
    SELECT NEW.message_id, NEW.target_kind, NEW.target_id
    FROM dm.messages AS message
    WHERE message.id = NEW.message_id
      AND (message.sender_kind, message.sender_id) <> (NEW.target_kind, NEW.target_id)
    ON CONFLICT DO NOTHING;
    RETURN NULL;
END;
$$;

CREATE TRIGGER actor_deliveries_record_message_recipient
AFTER INSERT ON dm.actor_deliveries
FOR EACH ROW
WHEN (NEW.delivery_kind = 'message' AND NEW.delivery_revision = 1)
EXECUTE FUNCTION dm_private.record_message_recipient();

-- Anything sent since 0047 by a service that did not record its recipients.
INSERT INTO dm.message_recipients (message_id, recipient_kind, recipient_id)
SELECT DISTINCT d.message_id, d.target_kind, d.target_id
FROM dm.actor_deliveries d
JOIN dm.messages m ON m.id = d.message_id AND m.conversation_id = d.conversation_id
WHERE d.delivery_kind = 'message'
  AND d.delivery_revision = 1
  AND (d.target_kind, d.target_id) <> (m.sender_kind, m.sender_id)
ON CONFLICT DO NOTHING;
