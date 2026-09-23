-- Public managed generation belongs to Honeycomb. Credential/lifecycle changes
-- need an independent internal fence even when that generation does not change.
ALTER TABLE dm.testing_environments
    ADD COLUMN runtime_revision bigint NOT NULL DEFAULT 1 CHECK (runtime_revision > 0);
UPDATE dm.testing_environments SET runtime_revision = version;
-- Existing managed generation drift is repaired only through live verified
-- discovery or an authenticated Honeycomb lifecycle operation, not guessed here.
