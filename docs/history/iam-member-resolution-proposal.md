# Application-scoped IAM member resolution

DM must be able to start a conversation with a valid organization member before that member's first DM sign-in. IAM client 1.2.1 currently provides only the authenticated caller's live authorization snapshot and consent-filtered webhook projections. The member and directory routes intentionally require first-party IAM sessions. Neither app Basic authentication nor a DM `oat_` session supplies a complete organization directory.

The current DM implementation uses fresh caller snapshots and verified member events. It supports known offline recipients and fails closed with 422 for absent or removed projections. This document proposes an upstream capability; it is not an implemented IAM route.

## Proposed contract

`POST /api/v1/application-directory/members/resolve`, authenticated with the receiving application's Basic credential and the same testing-environment header as every other SDK call.

```json
{
  "access_token": "<caller's application access token>",
  "org_id": "example-org",
  "participant_ids": ["another-carbon", "helper:example-org"]
}
```

The server validates the caller token, application audience, organization, environment, active membership, effective scopes, and current epochs together. Limit requests to 100 distinct public identifiers. Return only active and authorized members from that exact organization, with principal UUID, typed public ID, organization UUID/handle, membership UUID/version, and authorization epoch. Include no recipient credentials, contact information, private tags, roles, or trust data unless separately requested and authorized. Return a non-enumerating unavailable result for nonexistent, removed, or undisclosed members, and reject ambiguous untyped public IDs. An official typed SDK method should carry all credentials and environment bindings.

IAM must explicitly define which organization-level application authorization permits discovery of members who have never consented to or signed in to that app. The existing caller's `memberships.read` or `profile` scope must not silently grant access to other principals' data. A separate owner-approved installation grant or directory-read scope could authorize the minimal membership identity needed by organization messaging. Without such a grant, the endpoint must continue withholding those recipients, and the first-sign-in limitation remains part of the product contract.

The endpoint should bind response data to the authorization observation and expose membership versions so consumers can reconcile delayed webhooks safely. Application clients continue introspecting actual senders and receivers; member resolution never substitutes for the recipient's own authentication.

## Existing upstream evidence

IAM's `docs/INTEGRATION_FIXES_2026-09-05.md`, “First login and authorization-cache recovery,” explicitly limits bootstrap to the current access-token subject and retains the exclusion from first-party directory routes. Both member-list and directory-list handlers call `begin_organization`; `src/features/organizations/support.rs::direct_iam_binding` requires the `silicon-iam` audience, no application binding, and `iam.self`. The official SDK's `oauth.authorization` method wraps a single-token introspection rather than an arbitrary-member lookup.
