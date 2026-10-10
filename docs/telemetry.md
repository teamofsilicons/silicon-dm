# Diagnostics and analytics

DM records operational diagnostics by default. Turn them off in the website's account settings,
with `dm config telemetry false`, with `Client::with_telemetry(false)`, or with
`DM_TELEMETRY_ENABLED=false` (CLI, relay or service). `dm config telemetry true` turns the CLI and
relay back on.

## Where events go

Production events use the dedicated `tos / silicondm` table in
[Space Station](https://spacestation.teamofsilicons.com/o/tos/tables/silicondm).
Only the backend holds `DM_SPACE_STATION_TABLE_KEY`. It uses the official
`space-station` Rust package 0.1.1, whose local daemon spools records and ships them
with acknowledgements. The SDK, CLI and browser use DM's authenticated
`POST /api/v1/telemetry` endpoint; the ingest credential is never shipped in a
browser bundle or a CLI binary. A CLI command records its diagnostic in its home's
local queue and exits; the relay uploads it in the background (keeping at most
1,000 unsent events), so no command waits on the network for telemetry.

## What is captured

Events contain a schema version, DM version, source, event, environment and
process identity. HTTP events add the route template, method, request ID, response
status, success and duration. CLI completion, relay queue checks, and website page
views, request timings and error classifications provide client-side context.
Events never name an account.
Space Station adds system and ingestion metadata itself.

Group change events carry the group id, such as `g:product-design`, and counts.
The id includes the creation-name slug; names and descriptions are not recorded.

The website's analytics go through its own server and the same authenticated DM
API as product requests. DM does not use a third-party browser script or expose
the table key. Diagnostic input accepts a small fixed set of event types and
numeric/boolean fields. Message bodies, drafts, attachment links, short-lived tokens,
access and refresh tokens, app secrets, webhook bodies and exception strings are excluded.

## Operate the exporter

Set `DM_SPACE_STATION_TABLE_KEY` in the API service's secret environment and set
`DM_TELEMETRY_HOME` to a private writable durable directory. The default spool is
`/tmp/silicon-dm-telemetry`, which survives process restarts on the same filesystem
but not replacement of an ephemeral container. Mount persistent storage when
retaining unsent diagnostics across container replacement is required.

Production Fargate tasks use a private UID 10001, mode 0700 volume at
`/var/lib/silicon-dm/telemetry`. This volume lasts for the task; replacement
can discard unsent diagnostics.

Diagnostics are best effort and never gate message delivery. The official SDK
bounds its pending queue. A missing ingest key leaves production export inactive, while explicit
opt-out disables recording. Keep the Space Station daemon/spool accessible to the
backend process and inspect the dedicated table for delivery verification.

Useful query:

```sql
SELECT record.source, record.event, count() AS n
FROM silicondm
GROUP BY record.source, record.event
```

[Configuration](configuration.md) · [Version policy](contracts.md)

## Group diagnostics

Group routes participate in the same HTTP completion and client diagnostics as
messages. Successful group mutations emit `group.created`, `group.updated`,
`group.members_invited` and `group.invitations_removed`. Context is restricted
to `group_id`, `count` and `success`. These record successful requests, including
idempotent retries; do not count them as unique group creations. Names,
descriptions, invitee identities and message content never enter diagnostics.
Opt-out applies to these events too. The existing DM Overview window includes all group events.

```sql
SELECT record.event, count() AS observations
FROM silicondm
WHERE record.event LIKE 'group.%'
GROUP BY record.event
```

See [groups and membership](groups.md) for the full access model.
