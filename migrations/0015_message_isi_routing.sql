-- ISI selects an application route within an existing silicon account.
-- Actor membership, durable fanout and receipt sequences keep their canonical IDs.
ALTER TABLE dm.messages
    ADD COLUMN sender_address text,
    ADD COLUMN recipient_address text,
    ADD CONSTRAINT message_sender_address_length CHECK (sender_address IS NULL OR length(sender_address) BETWEEN 1 AND 255),
    ADD CONSTRAINT message_recipient_address_length CHECK (recipient_address IS NULL OR length(recipient_address) BETWEEN 1 AND 255);

CREATE FUNCTION dm_private.enforce_message_routing_immutable()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.sender_address IS DISTINCT FROM OLD.sender_address
        OR NEW.recipient_address IS DISTINCT FROM OLD.recipient_address THEN
        RAISE EXCEPTION 'message routing is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER message_routing_immutable BEFORE UPDATE ON dm.messages
FOR EACH ROW EXECUTE FUNCTION dm_private.enforce_message_routing_immutable();
REVOKE ALL ON FUNCTION dm_private.enforce_message_routing_immutable() FROM PUBLIC;
