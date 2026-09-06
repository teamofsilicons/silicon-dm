\if :{?runtime_role}
\else
\echo 'runtime_role psql variable is required'
\quit false
\endif

-- Run as the role that owns the migrated DM objects. psql identifier quoting
-- keeps the operator-supplied role name an identifier rather than SQL text.
GRANT USAGE ON SCHEMA dm TO :"runtime_role";
GRANT USAGE ON SCHEMA dm_private TO :"runtime_role";
-- Trigger bodies call these invariant validators under the runtime role.
-- Trigger-only functions do not need to be callable directly by that role.
GRANT EXECUTE ON FUNCTION dm_private.assert_conversation_participant_count(uuid) TO :"runtime_role";
GRANT EXECUTE ON FUNCTION dm_private.assert_message_content(uuid) TO :"runtime_role";
GRANT EXECUTE ON FUNCTION dm_private.assert_message_outbox_complete(uuid) TO :"runtime_role";
GRANT EXECUTE ON FUNCTION dm_private.assert_revision_outbox(uuid) TO :"runtime_role";
GRANT EXECUTE ON FUNCTION dm_private.assert_bundle_shape(uuid) TO :"runtime_role";
GRANT EXECUTE ON FUNCTION dm_private.assert_draft_attachment_limit(uuid, dm.actor_kind, text) TO :"runtime_role";
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA dm TO :"runtime_role";
-- Historical Hook sources remain owner-readable for retention and audit, but
-- the API/worker role has no product path that may read or mutate them.
REVOKE ALL PRIVILEGES ON TABLE dm.system_events FROM :"runtime_role";
GRANT SELECT ON TABLE public._sqlx_migrations TO :"runtime_role";

ALTER DEFAULT PRIVILEGES IN SCHEMA dm
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO :"runtime_role";
ALTER DEFAULT PRIVILEGES IN SCHEMA dm_private
    REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC;
