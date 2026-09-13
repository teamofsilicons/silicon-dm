CREATE TABLE dm.bug_reports (
    id uuid PRIMARY KEY,
    organization_id text NOT NULL,
    actor_id text NOT NULL,
    idempotency_key text NOT NULL,
    payload jsonb NOT NULL,
    status text NOT NULL CHECK (status IN ('queued','sent','simulated')),
    created_at timestamptz NOT NULL DEFAULT now(),
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    attempts integer NOT NULL DEFAULT 0,
    sent_at timestamptz,
    UNIQUE (organization_id, actor_id, idempotency_key)
);
CREATE INDEX bug_reports_pending ON dm.bug_reports(next_attempt_at) WHERE status = 'queued';
