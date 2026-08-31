\if :{?runtime_role}
\else
\echo 'runtime_role psql variable is required'
\quit false
\endif

-- Run as the role that owns the migrated DM objects. psql identifier quoting
-- keeps the operator-supplied role name an identifier rather than SQL text.
GRANT USAGE ON SCHEMA dm TO :"runtime_role";
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA dm TO :"runtime_role";
GRANT SELECT ON TABLE public._sqlx_migrations TO :"runtime_role";

ALTER DEFAULT PRIVILEGES IN SCHEMA dm
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO :"runtime_role";
ALTER DEFAULT PRIVILEGES IN SCHEMA dm_private
    REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
