-- Only authenticated event identities are retained. Never persist raw test envelopes,
-- whose outer key is root authority over the IAM test environment.
CREATE TABLE dm.iam_webhook_receipts (
    event_id uuid PRIMARY KEY,
    event_type text NOT NULL,
    occurred_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX iam_webhook_receipts_received_at_idx ON dm.iam_webhook_receipts (received_at);

-- A transactional generation avoids losing cross-process revalidation when event
-- inserts are concurrent. Each data plane has its own table via search_path.
CREATE TABLE dm.iam_authorization_revision (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    revision bigint NOT NULL DEFAULT 0
);
INSERT INTO dm.iam_authorization_revision (singleton) VALUES (true);
