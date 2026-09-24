-- IAM's cutover must pass its global collision preflight before this migration.
-- Only declared identity columns are rewritten. Signed/replay JSON, request
-- hashes, free text, ciphertext and private UUID keys are deliberately retained.
CREATE OR REPLACE FUNCTION pg_temp.schema_actor(value text) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
BEGIN
    IF value ~ '^si:[a-z0-9_-]{3,50}$' OR value ~ '^c:[a-z0-9_-]{3,30}$' THEN RETURN value; END IF;
    IF value ~ '^[a-z0-9_-]{3,50}:[a-z0-9_-]{3,50}$' THEN RETURN 'si:' || split_part(value, ':', 1); END IF;
    IF value ~ '^[a-z0-9_-]{3,30}$' THEN RETURN 'c:' || value; END IF;
    RAISE EXCEPTION 'unmapped public actor ID in schema cutover: %', value USING ERRCODE='22023';
END $$;
CREATE OR REPLACE FUNCTION pg_temp.schema_app(value text) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
BEGIN
    IF value ~ '^[a-z][a-z0-9_-]{0,79}$' THEN RETURN value; END IF;
    IF value ~ '^[a-z0-9_-]+>[a-z][a-z0-9_-]{0,79}$' THEN RETURN split_part(value, '>', 2); END IF;
    RAISE EXCEPTION 'unmapped application ID in schema cutover: %', value USING ERRCODE='22023';
END $$;

CREATE TEMP TABLE dm_schema_tables ON COMMIT DROP AS
SELECT c.oid,format('%I.%I',n.nspname,c.relname) AS relation,c.relforcerowsecurity AS forced
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname IN ('dm','dm_private') AND c.relkind='r';
CREATE TEMP TABLE dm_schema_triggers ON COMMIT DROP AS
SELECT tables.relation,t.tgname,t.tgenabled
FROM pg_trigger t JOIN dm_schema_tables tables ON tables.oid=t.tgrelid
WHERE NOT t.tgisinternal;
-- Historical rows can intentionally violate NOT VALID checks. Preserve their
-- exact definitions/state while changing only identity labels.
CREATE TEMP TABLE dm_schema_legacy_checks ON COMMIT DROP AS
SELECT conrelid::regclass::text AS relation,conname,pg_get_constraintdef(oid) AS definition
FROM pg_constraint WHERE contype='c' AND NOT convalidated
    AND conrelid IN(SELECT oid FROM dm_schema_tables);
CREATE TEMP TABLE dm_schema_fks ON COMMIT DROP AS
SELECT conrelid::regclass::text AS relation,conname,pg_get_constraintdef(oid) AS definition
FROM pg_constraint WHERE contype='f' AND conrelid IN(SELECT oid FROM dm_schema_tables);
DO $$ DECLARE r record; BEGIN
    FOR r IN SELECT * FROM dm_schema_tables ORDER BY relation LOOP
        EXECUTE format('LOCK TABLE %s IN ACCESS EXCLUSIVE MODE',r.relation);
        EXECUTE format('ALTER TABLE %s NO FORCE ROW LEVEL SECURITY',r.relation);
        EXECUTE format('ALTER TABLE %s DISABLE TRIGGER USER',r.relation);
    END LOOP;
    FOR r IN SELECT * FROM dm_schema_fks UNION ALL SELECT * FROM dm_schema_legacy_checks LOOP
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I',r.relation,r.conname);
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM dm.actor_snapshots WHERE
        (actor_kind='carbon' AND pg_temp.schema_actor(actor_id) NOT LIKE 'c:%') OR
        (actor_kind='silicon' AND pg_temp.schema_actor(actor_id) NOT LIKE 'si:%')) THEN
        RAISE EXCEPTION 'actor kind disagrees with public ID mapping' USING ERRCODE='22023';
    END IF;
    IF EXISTS(SELECT 1 FROM dm.ting_handoffs WHERE accepted_at IS NULL AND request_body IS NOT NULL) THEN
        RAISE EXCEPTION 'drain prepared DM Ting handoffs before the identifier cutover' USING ERRCODE='55000';
    END IF;
END $$;
-- Preserve the exact public strings originally authenticated by AES-GCM.
ALTER TABLE dm.ting_credentials ADD COLUMN aad_app_id text,ADD COLUMN aad_actor_id text;
UPDATE dm.ting_credentials SET aad_app_id=app_id,aad_actor_id=actor_id;
CREATE TEMP TABLE dm_schema_mapping(old_id text PRIMARY KEY,new_id text UNIQUE) ON COMMIT DROP;
DO $$ DECLARE r record; BEGIN
    FOR r IN SELECT c.table_schema,c.table_name,c.column_name FROM information_schema.columns c
        JOIN dm_schema_tables t ON t.relation=format('%I.%I',c.table_schema,c.table_name)
        WHERE c.data_type='text' AND c.column_name IN('actor_id','created_by_id','sender_id','recipient_id','target_id','target_silicon_id','carbon_id','originator_id','creator_actor_id')
    LOOP
        EXECUTE format('INSERT INTO dm_schema_mapping SELECT DISTINCT %I,pg_temp.schema_actor(%I) FROM %I.%I WHERE %I IS NOT NULL AND %I<>''honeycomb'' ON CONFLICT(old_id) DO NOTHING',r.column_name,r.column_name,r.table_schema,r.table_name,r.column_name,r.column_name);
    END LOOP;
    FOR r IN SELECT c.table_schema,c.table_name,c.column_name FROM information_schema.columns c
        JOIN dm_schema_tables t ON t.relation=format('%I.%I',c.table_schema,c.table_name)
        WHERE c.data_type='text' AND c.column_name IN('actor_id','created_by_id','sender_id','recipient_id','target_id','target_silicon_id','carbon_id','originator_id','creator_actor_id')
    LOOP
        EXECUTE format('UPDATE %I.%I row SET %I=m.new_id FROM dm_schema_mapping m WHERE row.%I=m.old_id',r.table_schema,r.table_name,r.column_name,r.column_name);
    END LOOP;
    FOR r IN SELECT c.table_schema,c.table_name,c.column_name FROM information_schema.columns c
        JOIN dm_schema_tables t ON t.relation=format('%I.%I',c.table_schema,c.table_name)
        WHERE c.data_type='text' AND c.column_name IN('app_id','iam_app_id')
    LOOP
        EXECUTE format('UPDATE %I.%I SET %I=pg_temp.schema_app(%I)',r.table_schema,r.table_name,r.column_name,r.column_name);
    END LOOP;
END $$;
UPDATE dm.iam_membership_projections SET membership_public_id=actor_id || '[' || organization_id || ']'
WHERE actor_id IS NOT NULL AND organization_id IS NOT NULL;
-- Rebuild only derived participant identities and private dedup indexes.
-- SHA-256 uses the same length-framed bytes as the application implementation.
WITH canonical AS (
    SELECT conversation_id,
        jsonb_agg(jsonb_build_object('type',actor_kind::text,'id',actor_id)
            ORDER BY actor_kind::text COLLATE "C",actor_id COLLATE "C") AS participants,
        sha256(string_agg(int8send(octet_length(actor_kind::text)::bigint) || convert_to(actor_kind::text,'UTF8') ||
            int8send(octet_length(actor_id)::bigint) || convert_to(actor_id,'UTF8'),''::bytea
            ORDER BY actor_kind::text COLLATE "C",actor_id COLLATE "C")) AS hash
    FROM dm.conversation_participants GROUP BY conversation_id
)
UPDATE dm.conversations c SET participant_set=canonical.participants,
    participant_set_fingerprint=sha256(convert_to(canonical.participants::text,'UTF8')),
    participant_set_hash=canonical.hash FROM canonical WHERE c.id=canonical.conversation_id
    AND NOT EXISTS(SELECT 1 FROM dm.groups g WHERE g.conversation_id=c.id);
UPDATE dm.ting_handoffs h SET public_conversation_id=a.public_id
FROM dm.conversation_addresses a WHERE a.id=h.conversation_id AND h.accepted_at IS NULL;
UPDATE dm.messages SET sender_address=CASE WHEN position('@' IN sender_address)>0
    THEN split_part(sender_address,'@',1) || '@' || pg_temp.schema_actor(split_part(sender_address,'@',2))
    ELSE pg_temp.schema_actor(sender_address) END WHERE sender_address IS NOT NULL;
UPDATE dm.messages SET recipient_address=CASE WHEN position('@' IN recipient_address)>0
    THEN split_part(recipient_address,'@',1) || '@' || pg_temp.schema_actor(split_part(recipient_address,'@',2))
    ELSE pg_temp.schema_actor(recipient_address) END WHERE recipient_address IS NOT NULL;
UPDATE dm.ting_handoffs SET routing_address=CASE WHEN position('@' IN routing_address)>0
    THEN split_part(routing_address,'@',1) || '@' || pg_temp.schema_actor(split_part(routing_address,'@',2))
    ELSE pg_temp.schema_actor(routing_address) END WHERE routing_address IS NOT NULL AND accepted_at IS NULL;
-- Already prepared Ting requests bind exact bytes to a proof/idempotency key:
-- drain these before cutover instead of mutating their recipient/body in place.

DO $$ DECLARE r record; BEGIN
    FOR r IN SELECT * FROM dm_schema_fks UNION ALL SELECT * FROM dm_schema_legacy_checks LOOP
        EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s',r.relation,r.conname,r.definition);
    END LOOP;
    FOR r IN SELECT * FROM dm_schema_tables LOOP
        IF r.forced THEN EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY',r.relation); END IF;
    END LOOP;
    FOR r IN SELECT * FROM dm_schema_triggers LOOP
        EXECUTE format('ALTER TABLE %s %s TRIGGER %I',r.relation,
            CASE r.tgenabled WHEN 'D' THEN 'DISABLE' WHEN 'R' THEN 'ENABLE REPLICA'
                WHEN 'A' THEN 'ENABLE ALWAYS' ELSE 'ENABLE' END,r.tgname);
    END LOOP;
END $$;
