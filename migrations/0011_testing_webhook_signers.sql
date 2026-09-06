-- Dedicated IAM test webhook registrations may have their own signing credentials.
ALTER TABLE dm.testing_environments
    ADD COLUMN iam_webhook_secret_ciphertext text,
    ADD COLUMN iam_webhook_key_version bigint,
    ADD CONSTRAINT testing_webhook_signer_pair CHECK (
        (iam_webhook_secret_ciphertext IS NULL) = (iam_webhook_key_version IS NULL)
        AND (iam_webhook_key_version IS NULL OR iam_webhook_key_version > 0)
    );
