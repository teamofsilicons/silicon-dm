-- Isolated diagnostic records are copied into each sandbox and removed on clean.
CREATE TABLE dm.telemetry_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    occurred_at timestamptz NOT NULL DEFAULT now(),
    event jsonb NOT NULL
);
CREATE INDEX telemetry_events_time ON dm.telemetry_events(occurred_at);
