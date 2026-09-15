-- Public group addresses are immutable; UUID foreign keys retain existing history.
CREATE FUNCTION dm.group_slug(p_name text) RETURNS text LANGUAGE sql IMMUTABLE STRICT AS $$
 SELECT trim(both '-' FROM regexp_replace(lower(p_name COLLATE "C"), '[^a-z0-9]+', '-', 'g'));
$$;
ALTER TABLE dm.groups ADD COLUMN public_id text;
-- Preserve every existing group, including duplicate or non-ASCII-only names.
-- Allocate deterministic numeric suffixes only during this migration.
DO $$
DECLARE g record; base text; candidate text; suffix integer;
BEGIN
 FOR g IN SELECT conversation_id,organization_id,name FROM dm.groups ORDER BY conversation_id LOOP
  base := 'g:' || g.organization_id || ':' || COALESCE(NULLIF(dm.group_slug(g.name), ''), 'group');
  candidate := base; suffix := 1;
  WHILE EXISTS (SELECT 1 FROM dm.groups WHERE public_id=candidate) LOOP
   suffix := suffix + 1; candidate := base || '-' || suffix;
  END LOOP;
  UPDATE dm.groups SET public_id=candidate WHERE conversation_id=g.conversation_id;
 END LOOP;
END;
$$;
ALTER TABLE dm.groups ALTER COLUMN public_id SET NOT NULL;
ALTER TABLE dm.groups ADD CONSTRAINT groups_public_id_unique UNIQUE(public_id);
CREATE FUNCTION dm.assign_group_public_id() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE slug text;
BEGIN
 IF TG_OP='UPDATE' THEN
  IF NEW.public_id IS DISTINCT FROM OLD.public_id OR NEW.organization_id IS DISTINCT FROM OLD.organization_id
     OR NEW.conversation_id IS DISTINCT FROM OLD.conversation_id THEN
   RAISE EXCEPTION 'group identity is immutable' USING ERRCODE='23514';
  END IF;
 ELSE
  slug := dm.group_slug(NEW.name);
  IF slug='' THEN
   RAISE EXCEPTION 'group name must contain an ASCII letter or digit' USING ERRCODE='23514';
  END IF;
  IF NEW.organization_id !~ '^[a-zA-Z0-9][a-zA-Z0-9_-]*$' THEN
   RAISE EXCEPTION 'organization ID cannot form a group address' USING ERRCODE='23514';
  END IF;
  NEW.public_id := 'g:' || NEW.organization_id || ':' || slug;
 END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER groups_public_identity BEFORE INSERT OR UPDATE ON dm.groups
 FOR EACH ROW EXECUTE FUNCTION dm.assign_group_public_id();
