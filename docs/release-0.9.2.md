# DM 0.9.2

Durable message events and local webhook callbacks expose identical delivery metadata
at both `metadata` and `data.metadata`. Existing nested consumers keep working, and
Silicon interpreters that require top-level metadata can receive messages again.
The local relay upgrades already queued callbacks before retrying them.

Deploy the backend and update the CLI/relay together. No database migration or
contract-version change is required for this additive compatibility fix.
