-- Membership authority comes only from current IAM introspection or signed events.
-- Tombstones retain UUID/version even when scope filtering withholds public IDs.
CREATE TABLE dm.iam_membership_projections (
    membership_id uuid PRIMARY KEY,
    principal_id uuid NOT NULL,
    iam_organization_id uuid NOT NULL,
    organization_id text,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text,
    iam_version bigint NOT NULL CHECK (iam_version > 0),
    authorization_epoch bigint CHECK (authorization_epoch >= 0),
    status text NOT NULL CHECK (status IN ('active', 'removed')),
    refreshed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK (status <> 'active' OR (organization_id IS NOT NULL AND actor_id IS NOT NULL AND authorization_epoch IS NOT NULL))
);
CREATE INDEX iam_membership_directory_lookup ON dm.iam_membership_projections
    (organization_id, actor_id) WHERE status = 'active';
CREATE INDEX iam_membership_principal_lookup ON dm.iam_membership_projections
    (iam_organization_id, principal_id);
