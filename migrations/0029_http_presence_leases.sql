-- Presence is an authenticated HTTP lease, independent of Ting's transport.
CREATE TABLE dm.client_presence_leases (
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    device_id text NOT NULL,
    heartbeat_at timestamptz NOT NULL,
    lease_expires_at timestamptz NOT NULL,
    activity dm.presence_activity,
    activity_expires_at timestamptz,
    disconnected_at timestamptz,
    last_seen_at timestamptz,
    PRIMARY KEY (organization_id, actor_kind, actor_id, device_id),
    FOREIGN KEY (organization_id, actor_kind, actor_id)
        REFERENCES dm.actor_snapshots (organization_id, actor_kind, actor_id)
        ON DELETE CASCADE,
    CHECK (octet_length(device_id) BETWEEN 1 AND 255 AND device_id !~ '[[:cntrl:]]'),
    CHECK (lease_expires_at > heartbeat_at),
    CHECK ((activity IS NULL) = (activity_expires_at IS NULL)),
    CHECK (activity_expires_at IS NULL OR activity_expires_at > heartbeat_at)
);
CREATE INDEX client_presence_leases_expiry_idx
    ON dm.client_presence_leases (lease_expires_at)
    WHERE disconnected_at IS NULL;
COMMENT ON TABLE dm.client_presence_leases IS
    'Per-actor and device HTTP presence only; no delivery sessions, queues or credentials.';
REVOKE ALL ON TABLE dm.client_presence_leases FROM PUBLIC;
