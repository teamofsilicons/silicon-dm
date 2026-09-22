-- Every message creation, revision and receipt fan-out wakes Ting workers only
-- after its source transaction commits. Identical schema payloads coalesce
-- within a transaction; workers always read authoritative rows rather than
-- trusting notification contents, and polling covers missed notifications.
CREATE FUNCTION dm_private.notify_ting_handoff() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('dm_delivery',TG_TABLE_SCHEMA);
    RETURN NEW;
END;
$$;
CREATE TRIGGER ting_handoffs_notify AFTER INSERT ON dm.ting_handoffs
    FOR EACH ROW EXECUTE FUNCTION dm_private.notify_ting_handoff();
REVOKE ALL ON FUNCTION dm_private.notify_ting_handoff() FROM PUBLIC;
