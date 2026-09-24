-- DM enrolls every member with Ting automatically; delivery is not a member choice.
-- Ting only accepts a registration proven by the recipient's own IAM session, so
-- enrollment runs whenever DM holds one: login, any authenticated request, or a
-- cached access token found by the worker's sweep. A row without enrolled_at is
-- retried after its cooldown. Ting rejecting a send as recipient_not_registered
-- deletes the row so the next verified session enrolls the member again.
CREATE TABLE dm.ting_automatic_enrollments (
    app_id text NOT NULL CHECK (octet_length(app_id) BETWEEN 1 AND 255),
    generation bigint NOT NULL CHECK (generation >= 0),
    organization_id text NOT NULL,
    actor_kind dm.actor_kind NOT NULL,
    actor_id text NOT NULL,
    subscription_id text CHECK (subscription_id IS NULL OR octet_length(subscription_id) BETWEEN 1 AND 255),
    enrolled_at timestamptz,
    attempted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_error text CHECK (last_error IS NULL OR octet_length(last_error) BETWEEN 1 AND 128),
    PRIMARY KEY (app_id, generation, organization_id, actor_kind, actor_id),
    CHECK ((enrolled_at IS NULL) = (subscription_id IS NULL))
);
CREATE INDEX ting_automatic_enrollments_pending
    ON dm.ting_automatic_enrollments (attempted_at) WHERE enrolled_at IS NULL;
COMMENT ON TABLE dm.ting_automatic_enrollments IS
    'Automatic Ting recipient enrollment per app, generation, organization and typed actor; holds no credentials.';
