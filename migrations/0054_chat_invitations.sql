CREATE TABLE dm.chat_invitations (
    id uuid PRIMARY KEY,
    inviter_uuid text NOT NULL REFERENCES dm.accounts(uuid),
    partner_uuid text NOT NULL REFERENCES dm.accounts(uuid),
    recipient_uuid text REFERENCES dm.accounts(uuid),
    recipient_email text,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'accepted', 'declined')),
    conversation_id uuid REFERENCES dm.conversations(id),
    answered_by text REFERENCES dm.accounts(uuid),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL DEFAULT now() + interval '14 days',
    mail_status text NOT NULL CHECK (mail_status IN ('none', 'simulated', 'queued', 'sent')),
    mail_attempts integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    CHECK (num_nonnulls(recipient_uuid, recipient_email) = 1),
    CHECK (recipient_uuid IS DISTINCT FROM partner_uuid),
    CHECK ((status = 'accepted') = (conversation_id IS NOT NULL)),
    CHECK ((status = 'pending') = (answered_by IS NULL))
);
CREATE INDEX chat_invitations_recipient ON dm.chat_invitations(recipient_uuid, created_at DESC) WHERE status = 'pending';
CREATE INDEX chat_invitations_email ON dm.chat_invitations(recipient_email, created_at DESC) WHERE status = 'pending';
CREATE INDEX chat_invitations_sender ON dm.chat_invitations(inviter_uuid, created_at DESC);
CREATE INDEX chat_invitations_mail ON dm.chat_invitations(next_attempt_at) WHERE mail_status = 'queued' AND status = 'pending';
