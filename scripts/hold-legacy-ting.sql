-- Run only with all DM writers stopped and a verified database/configuration backup.
-- The caller supplies cutover_manifest(delivery_id,body_sha256) and transaction-local
-- dm.cutover.batch / dm.cutover.backup settings. This file does not commit.
LOCK TABLE dm.ting_handoffs IN ACCESS EXCLUSIVE MODE;
CREATE SCHEMA IF NOT EXISTS dm_cutover_hold;
REVOKE ALL ON SCHEMA dm_cutover_hold FROM PUBLIC;
CREATE TABLE IF NOT EXISTS dm_cutover_hold.ting_handoffs (
    delivery_id uuid PRIMARY KEY,
    batch_id text NOT NULL,
    state text NOT NULL CHECK (state = 'held'),
    held_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    backup_receipt text NOT NULL CHECK (length(backup_receipt) > 0),
    original_row jsonb NOT NULL,
    request_bytes bytea NOT NULL,
    body_sha256 bytea NOT NULL,
    row_sha256 bytea NOT NULL,
    CHECK (sha256(request_bytes) = body_sha256),
    CHECK (convert_to(original_row->>'request_body','UTF8') = request_bytes),
    CHECK ((original_row->>'delivery_id')::uuid = delivery_id),
    CHECK (sha256(convert_to(original_row::text,'UTF8')) = row_sha256)
);
REVOKE ALL ON dm_cutover_hold.ting_handoffs FROM PUBLIC;
-- Remove inherited default grants as well as PUBLIC access; the operator owns
-- this archive, and application runtime roles must never read or replay it.
DO $$ DECLARE r record; BEGIN
    FOR r IN SELECT DISTINCT p.rolname FROM pg_class c
        CROSS JOIN LATERAL aclexplode(c.relacl) a JOIN pg_roles p ON p.oid=a.grantee
        WHERE c.oid='dm_cutover_hold.ting_handoffs'::regclass AND a.grantee<>c.relowner
    LOOP EXECUTE format('REVOKE ALL ON dm_cutover_hold.ting_handoffs FROM %I',r.rolname); END LOOP;
    FOR r IN SELECT DISTINCT p.rolname FROM pg_namespace n
        CROSS JOIN LATERAL aclexplode(n.nspacl) a JOIN pg_roles p ON p.oid=a.grantee
        WHERE n.nspname='dm_cutover_hold' AND a.grantee<>n.nspowner
    LOOP EXECUTE format('REVOKE ALL ON SCHEMA dm_cutover_hold FROM %I',r.rolname); END LOOP;
END $$;
DO $$
DECLARE expected bigint; active bigint; retained bigint; moved bigint;
    batch text := current_setting('dm.cutover.batch');
    backup text := current_setting('dm.cutover.backup');
BEGIN
    SELECT count(*) INTO expected FROM cutover_manifest;
    IF expected=0 OR length(batch)=0 OR length(backup)=0 THEN
        RAISE EXCEPTION 'exact nonempty manifest, batch and backup receipt are required';
    END IF;
    SELECT count(*) INTO active FROM dm.ting_handoffs h JOIN cutover_manifest m USING(delivery_id);
    SELECT count(*) INTO retained FROM dm_cutover_hold.ting_handoffs h
        JOIN cutover_manifest m USING(delivery_id)
        WHERE h.batch_id=batch AND h.body_sha256=m.body_sha256 AND h.state='held';
    IF active=0 AND retained=expected AND
       (SELECT count(*) FROM dm_cutover_hold.ting_handoffs WHERE batch_id=batch)=expected THEN
        RETURN; -- exact completed hold is idempotent, including after migration
    END IF;
    IF active<>expected OR retained<>0 OR EXISTS(
        SELECT 1 FROM dm_cutover_hold.ting_handoffs WHERE batch_id=batch) THEN
        RAISE EXCEPTION 'manifest differs from the exact active/held batch';
    END IF;
    IF EXISTS(SELECT 1 FROM public._sqlx_migrations WHERE version>=36 AND success) THEN
        RAISE EXCEPTION 'new holds must be prepared before the public identifier migration';
    END IF;
    IF EXISTS(SELECT 1 FROM dm.ting_handoffs h JOIN cutover_manifest m USING(delivery_id)
        WHERE h.accepted_at IS NOT NULL OR h.ting_id IS NOT NULL OR h.ting_created_at IS NOT NULL
        OR h.lease_id IS NOT NULL OR h.lease_owner IS NOT NULL OR h.lease_expires_at IS NOT NULL
        OR sha256(convert_to(h.request_body,'UTF8')) IS DISTINCT FROM m.body_sha256) THEN
        RAISE EXCEPTION 'a handoff changed, was accepted, is unprepared or is still leased';
    END IF;
    IF (SELECT count(*) FROM dm.ting_handoffs WHERE accepted_at IS NULL)<>expected THEN
        RAISE EXCEPTION 'additional unaccepted work (including unprepared rows) is outside the exact reviewed manifest';
    END IF;
    INSERT INTO dm_cutover_hold.ting_handoffs
        (delivery_id,batch_id,state,backup_receipt,original_row,request_bytes,body_sha256,row_sha256)
    SELECT h.delivery_id,batch,'held',backup,to_jsonb(h),convert_to(h.request_body,'UTF8'),
        m.body_sha256,sha256(convert_to(to_jsonb(h)::text,'UTF8'))
    FROM dm.ting_handoffs h JOIN cutover_manifest m USING(delivery_id);
    IF EXISTS(SELECT 1 FROM dm.ting_handoffs h JOIN cutover_manifest m USING(delivery_id)
        JOIN dm_cutover_hold.ting_handoffs a USING(delivery_id)
        WHERE a.original_row IS DISTINCT FROM to_jsonb(h)
        OR a.request_bytes IS DISTINCT FROM convert_to(h.request_body,'UTF8')) THEN
        RAISE EXCEPTION 'archive verification failed';
    END IF;
    -- This is a transactional transfer to the held queue, not cancellation or
    -- delivery. Source messages, actor deliveries and receipts are untouched.
    DELETE FROM dm.ting_handoffs h USING cutover_manifest m WHERE h.delivery_id=m.delivery_id;
    GET DIAGNOSTICS moved=ROW_COUNT;
    IF moved<>expected THEN RAISE EXCEPTION 'hold transfer count changed'; END IF;
END $$;
SELECT json_build_object('state','held','batch',current_setting('dm.cutover.batch'),
    'held_count',(SELECT count(*) FROM dm_cutover_hold.ting_handoffs WHERE batch_id=current_setting('dm.cutover.batch')),
    'active_prepared_count',(SELECT count(*) FROM dm.ting_handoffs WHERE accepted_at IS NULL AND request_body IS NOT NULL));
