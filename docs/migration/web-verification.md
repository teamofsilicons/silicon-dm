# DM website verification — 2026-10-10

The Solid/Vite frontend and browser token gateway are replaced with the shared Next.js 16 / React 19 / Arc UI foundation. Product pages cover conversations, cross-device drafts, durable retryable local outbox, receipts, presence/sync, attachments/audio/video/transcripts, GIF lookup, message edits/history/deletion/replies, Silicon bundles and their originals, invitation/owner management, custodian read views, Silicon allow-lists and reports. Tokens remain in a sealed HttpOnly cookie; the BFF forwards only the app API request and server-held bearer.

## Observed evidence

- Real backend/CLI Accounts suite: **151/151**, `.mig/e2e/run-617021/result.json` and transcript. Carbon hosted/code and CLI device flows, Silicon SLT, custody/sharing, signed lifecycle webhook/replay refusal, proof calls and restart were exercised.
- Frontend TypeScript, ESLint, **44 unit tests**, and Next production standalone build passed. Logs under `.mig/logs/web-{typecheck,lint,unit,build}.log`.
- Browser full run: **24 generic checks passed**; its product check exposed an empty voice transcript issue. After fixing it, the complete two-Carbon product journey plus setup passed **2/2**, `.mig/logs/web-browser-product.log`. This covers send, read, edit, draft after reload, invitation acceptance, group settings, report, CSRF refusal, identity and sign-out. The generic checks covered real hosted login, refresh/replay, logout across tabs, public routes, shell keyboard and WCAG 2.2 AA in light/dark at 1440×900 and 390×844.
- Screenshots in ignored `web/screens/` cover the public/signed-out states, every main section and populated conversation/group in all four theme/viewport combinations. Visually inspected populated desktop/light and phone/dark pages; no horizontal overflow in the product captures.
- New `deploy/aws/web.yaml` passes cfn-lint 1.57.2. `web/Dockerfile.gateway` builds the standalone server and retains the release artifact name. The old gateway stack is preserved for rollback, with explicit retirement documentation.

## Scope and release gates

All evidence above is local. No push, tag, registry upload, production database or DNS change was performed. The Docker daemon did not answer the local info request, so the container image itself was not built here; CI must build it. CloudFormation changes and public-origin sign-in/messaging remain coordinated cutover gates. The website runs one replica until refresh rotation has shared coordination. The local proof recipient is a strict Accounts-verifying stand-in; this is not verification of live Ting or Interface deployment.

Human-owned `UNDERSTANDING.md` and original checkouts are unchanged. Browser outputs and credentials are ignored and not committed.
