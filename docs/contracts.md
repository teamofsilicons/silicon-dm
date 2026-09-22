# Contracts and compatibility

DM serves HTTP contract 3. Public `/api/v1/ws` and `/api/v1/ws/shared` routes are
retired and return HTTP 410 `delivery_moved_to_ting`; WebSocket 5 and shared
transport 2 are no longer supported. Upgrade the backend and all delivery
consumers together. Clients still using DM sockets cannot receive live updates
from the migrated backend.

`GET /api/v1/contracts` returns the selected data plane's contract lifecycle,
HTTP-only compatibility, retired routes and Ting delivery discovery. The same
`delivery` object appears in `/api/v1/iam`: Ting's API URL, browser origin,
separate receiver-session requirement, originating-DM-session publisher authority,
and DM registration/sync/presence/receipt paths. These values describe the
integration; they do not certify live Ting configuration or successful delivery.

HTTP responses advertise `X-DM-Contract-Version: 3`; they no longer advertise a
DM socket protocol. Unsupported explicit HTTP versions return 406. Retired
socket paths return 410 regardless of old protocol headers. Testing planes
retain independent contract usage. Message history, group versions and draft
versions remain independent of notification transport.

Use Ting for updates and DM HTTP for mutations, receipt writes, device presence
and cursor recovery. Preserve the opaque `/sync` cursor; a Ting sequence is not
its replacement. See [API delivery migration](api/README.md#ting-enrollment-and-http-recovery),
[Ting integration requirements](ting-integration-issues.md), and
[wire format](wire-format.md).
