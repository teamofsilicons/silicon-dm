-- Control metadata only; never copy these tables into a sandbox schema.
ALTER TABLE dm.testing_environments
    ADD COLUMN iam_control_version bigint,
    ADD COLUMN iam_cleaned_at timestamptz,
    ADD COLUMN iam_sync_pending boolean NOT NULL DEFAULT false;
