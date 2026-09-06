# Silicon DM integration guides

- [API](api/README.md): HTTP paths, authentication, content, errors, WebSocket frames, ACKs, and pagination.
- [Rust client](client/README.md): stateless typed methods and caller-owned credentials.
- [Optional SDK runtime](client/runtime.md): durable local relay hosting, daemon launch, and hourly update policy.
- [CLI](cli/README.md): command discovery, profiles, short-lived-token login, local daemon, and durable relay.
- [IAM](iam.md): application sessions, organization authority, signed webhooks, and revocation.
- [Testing environments](testing-environments.md): isolated IAM pairing, root keys, lifecycle, and recovery.
- [Deployment](deployment.md): production configuration, database roles, image startup, ingress, manual acceptance, and client releases.
- [OpenAPI](../openapi.yaml): machine-readable HTTP and realtime schema reference.
- [Manual verification](manual-backend-verification.md): actual backend, Rust-client, CLI, IAM and sandbox checks, with their practical limits.

Application credentials belong only on the backend. An end user provides an IAM
short-lived token to DM login and uses the resulting application session. Test
root keys select a sandbox; ordinary user actions still require that sandbox's
IAM-issued actor session. DM exposes no OBO routes.
