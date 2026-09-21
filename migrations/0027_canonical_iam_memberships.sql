-- DM owns the UUID row key. IAM's public identity and membership handles are
-- canonical strings; the old IAM UUID is retained only to correlate old events.
ALTER TABLE dm.iam_membership_projections
    ADD COLUMN membership_public_id text,
    ADD COLUMN iam_membership_id uuid;
UPDATE dm.iam_membership_projections
SET membership_public_id = actor_id || '[' || organization_id || ']',
    iam_membership_id = membership_id;
CREATE UNIQUE INDEX iam_membership_public_identity
    ON dm.iam_membership_projections(membership_public_id);
CREATE UNIQUE INDEX iam_membership_resource_identity
    ON dm.iam_membership_projections(iam_membership_id);
-- This historical column is no longer read or written by DM authorization.
ALTER TABLE dm.iam_membership_projections ALTER COLUMN principal_id DROP NOT NULL;
