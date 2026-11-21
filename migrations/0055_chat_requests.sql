CREATE TABLE dm.direct_preferences (
    owner text PRIMARY KEY REFERENCES dm.accounts(uuid),
    contacts_only boolean NOT NULL DEFAULT false
);
CREATE TABLE dm.chat_requests (
    id uuid PRIMARY KEY,
    recipient text NOT NULL REFERENCES dm.accounts(uuid),
    sender text NOT NULL REFERENCES dm.accounts(uuid),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'accepted', 'declined')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (recipient, sender),
    CHECK (recipient <> sender)
);
CREATE INDEX chat_requests_inbox ON dm.chat_requests (recipient, created_at DESC, id) WHERE status = 'pending';
CREATE INDEX chat_requests_outgoing ON dm.chat_requests (sender, created_at);
