# Contracts and compatibility

DM serves HTTP contract 4 (DM 0.15): every request sends `x-dm-contract-version: 4` and every
response advertises it. Contract 4 signs every account in with Silicon Accounts: each account
stands on its own, named by uuid and current id, with groups that have owners and invitations,
Silicon allow-lists, `GET /me`, `GET /accounts` and `GET /sync?wait=`. A client on
contract 3 gets 406 `unsupported_contract`; upgrade the client and the service together.

`GET /api/v1/contracts` (no sign-in) returns the supported versions, their lifecycle, the retired
routes (so an old client learns why a path now answers 404 or 410) and how updates are delivered.
`GET /api/v1/accounts` (no sign-in) says how to sign in to this DM service.

The retired WebSocket paths (`/api/v1/ws`, `/api/v1/ws/shared`) answer 410 `delivery_moved`
regardless of headers. Updates come from the sync feed, or through Ting where the service is
configured for it ([receiving](cli/receiving.md)).

Compatibility rules within a contract:

- New optional response fields can appear at any time; ignore what you do not know.
- Request fields and routes are not removed or changed in meaning without a new contract
  version. A deprecated version (announced in `GET /contracts`) is retired once it has gone
  unused for seven days; requests on it then get 410 `contract_sunset`.
- Message codes, group ids and conversation addresses are stable identifiers; an account's id can
  change, its uuid never does.
- Sync cursors are opaque and bound to the account; keep them and pass them back unchanged.

See the [wire format](wire-format.md) and the [OpenAPI document](../openapi.yaml).
