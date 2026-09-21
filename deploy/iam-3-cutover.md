# IAM canonical identity cutover

Deploy this adapter before IAM 3. It reads current IAM responses with UUID
membership keys and extra principal fields, and canonical IAM responses without
principal fields. Authentication uses the verified public actor handle, actor
type, organization, audience, environment, scopes, membership and live epoch.

Apply migration 0027 before starting the new backend. Existing directory UUID
rows and conversation/message data remain unchanged. The adapter binds canonical
membership handles to those rows; new rows get DM-owned private keys when IAM
does not expose a resource UUID. Signed membership events retain their resource
UUID and carry a canonical membership handle, including removal tombstones.
Older signed events remain readable. Epoch/version ordering still prevents stale
live snapshots or deliveries from undoing a removal.

DM's existing `/auth/me` response keeps its legacy `principal_id` string solely
for installed DM clients. Its value is the canonical `member.id`, not an IAM UUID
or an additional identity. No authorization uses that response alias. This avoids
an unrelated DM client protocol break during IAM's deployment.

Back up both DM database planes before migration. Keep this compatible backend
if IAM rolls back. A rollback to the old backend after new DM-owned row keys have
been allocated requires a database restore and review of writes since the backup.
Do not roll back the executable alone after canonical IAM begins serving traffic.

Validation covers current/canonical snapshots retaining one directory row,
canonical removal tombstones, stale authorization rejection, existing group
access transitions, unit tests and strict all-target Clippy. The migration adds
columns to an existing table covered by the existing runtime table grants.
