-- Accept canonical 128-bit Accounts UUIDs while legacy keys are being remapped.
ALTER TABLE dm.accounts DROP CONSTRAINT accounts_uuid_format;
ALTER TABLE dm.accounts ADD CONSTRAINT accounts_uuid_format CHECK(uuid ~ '^([A-Za-z0-9]{3,64}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$');
ALTER TABLE dm.accounts DROP CONSTRAINT accounts_custodian_format;
ALTER TABLE dm.accounts ADD CONSTRAINT accounts_custodian_format CHECK(custodian_uuid IS NULL OR custodian_uuid ~ '^([A-Za-z0-9]{3,64}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$');
ALTER TABLE dm.identity_links DROP CONSTRAINT identity_links_uuid_format;
ALTER TABLE dm.identity_links ADD CONSTRAINT identity_links_uuid_format CHECK(accounts_uuid ~ '^([A-Za-z0-9]{3,64}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$');
