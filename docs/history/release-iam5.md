# Coordinated IAM 5 release

This release separates feature OBO consent from ordinary login, refreshes the
vendored IAM client to the exact 5.0.0 release commit, and retires implicit
login-derived delegated authority. Existing users keep their ordinary sessions
and authorize delegated features when needed.

IAM client source: `f1e9c4768029aacabe337ca41be52e05023d1631`.
See `vendor/silicon-iam-client/VENDORED.md` for package provenance.

Production rollout is coordinated with IAM 5 and the receiving providers. Build
artifacts are candidates until integration checks and database backups pass.

The browser offers separate Carbon and Silicon popups plus typed full-page links
when popups are unavailable. Login state pins the initiating saved profile and
expected actor kind. Completion carries only an opaque profile ID and nonce;
the tab verifies that exact profile before selecting it. Switching accounts or
closing the sign-in view prevents a late result from retargeting the tab.

The gateway retains an HMAC-bound callback receipt with the saved profile so a
lost response or gateway restart can replay the same callback without another
exchange. Temporary failures expose explicit retries; no SLT is stored in browser
storage. Provider feature consent remains separate from ordinary sign-in.
