---
title: Configuration
description: Every setting of the dm command, the DM service and the website, with its default and what it is for. Speech, sign-in, Ting, GIFs, the database, timeouts, workers and bug-report mail.
kind: reference
order: 67
related:
  - reference/limits.md
  - operations/deployment.md
  - reference/cli.md
  - learn/voices-and-speech.md
---

# Configuration

DM ships defaults for ordinary use, so most deployments set only the secrets and URLs below. Change a setting
explicitly when your machine or deployment needs something else. The service reads its settings from the environment
at start and refuses to start, naming the variable and the reason, when one is missing, malformed or unsafe;
`.env.example` at the repository root lists every service setting with its default.

## The dm command

| Setting | Default | Purpose |
| --- | --- | --- |
| `DM_API_URL` / `dm login --api-url` | `https://api.dm.teamofsilicons.com` | The DM service a profile signs in to (remembered by the profile) |
| `ACCOUNTS_URL` / `dm login --accounts-url` | `https://accounts.teamofsilicons.com` | The Silicon Accounts that signs DM in (remembered by the profile) |
| `DM_APP_ID` | `dm` | DM's app id at Silicon Accounts; only for a development app |
| `SILICON_DM_HOME` | unset | An explicit, absolute state directory (takes precedence) |
| `dm config home LOCATION` | unset | Keep state under `LOCATION/.silicon-dm` |
| `SILICON_HOME` | the operating-system home | Parent of `.silicon-dm` when nothing above is set |
| `SILICON_DM_RELAY_HOME` | `~/.silicon-dm/relay` of the operating-system user | Where the shared relay keeps its files |
| `--profile NAME` / `dm profiles use` | `default` | Which signed-in account a command uses |
| `--wait-seconds N` | 30 | How long a command waits for the relay; on timeout the request stays queued |
| `dm daemon start --port PORT` | the last port used, else 19780 | The relay's preferred loopback port |

DM records no telemetry.

Plain `http` URLs are accepted only for this machine (`localhost`, `*.localhost`, loopback addresses). Keep the state
directory on a durable local disk: it holds the sign-ins and the outgoing queue. One relay serves every state directory
of the operating-system user.

`dm accounts --json` shows the values in effect without signing in. Setting `SILICON_DM_TEST` or `DM_TEST_APP_SECRET`
(test environments, retired) makes every networked command refuse with `testing_environments_retired`, so a home that
still selects a test environment never reaches the live service.

### Installing and updating

Silicon Apps installs and updates the command: `silicon-apps install dm` (development releases:
`silicon-apps install 'dm>dev'`). DM never replaces its own executable. A running relay keeps serving after an
update; the next `dm` command from the new version replaces an older relay, and queued requests survive. Rust programs
manage `silicon-dm-client` as an ordinary dependency.

## The service

### Sign-in

Sign-in is Silicon Accounts only:

| Variable | Required | Purpose |
| --- | --- | --- |
| `ACCOUNTS_URL` | yes | Silicon Accounts origin; exactly the `iss` of its tokens |
| `ACCOUNTS_API_URL` | no | Where DM calls Silicon Accounts server to server (default `ACCOUNTS_URL`) |
| `DM_APP_ID` | no | DM's app id in Silicon Accounts (default `dm`) |
| `DM_APP_SECRET` | yes | DM's app secret |
| `DM_ACCOUNTS_WEBHOOK_SECRET` | yes | The app webhook signing secret (`whsec_…`); webhooks arrive at `/webhook/` |
| `DM_ACCOUNTS_REQUEST_TIMEOUT_SECONDS` | no | Timeout of each call to Silicon Accounts (default 5) |
| `DM_DATA_KEY` | yes | 32 random bytes, base64 (`openssl rand -base64 32`): keys sync cursors and the encryption of stored proofs. Keep it stable and backed up |
| `DM_PROOF_ISSUERS` | no | Apps whose User verification proofs DM accepts, per scope (`dm.messages.write=interface,…`); empty accepts none |

DM's sign-in setup at Silicon Accounts needs `device_flow` and `public_client` turned on for the command, and
requires no account details (email and timezone are optional).

### The server and the database

| Variable | Default | Purpose |
| --- | --- | --- |
| `DM_ENVIRONMENT` | `development` | `development`, `test` or `production`. Production requires HTTPS upstreams and a verified database connection |
| `DM_BIND_ADDR` | `127.0.0.1:8080` | Where `dm-api` listens |
| `DM_PUBLIC_BASE_URL` | required | The public API base, such as `https://api.dm.teamofsilicons.com/api/v1` |
| `DM_REQUEST_TIMEOUT_SECONDS` | `15` | Ordinary requests (the sync long-poll, speech and transcription have their own, longer limits) |
| `DM_SHUTDOWN_TIMEOUT_SECONDS` | `30` | Graceful shutdown deadline |
| `DM_MAX_HTTP_BODY_BYTES` | `134217728` (128 MiB) | Largest request body (1 byte to 3 GiB) |
| `DM_DATABASE_URL` | required | PostgreSQL; production needs exactly one `sslmode=verify-full` |
| `DM_DATABASE_MAX_CONNECTIONS`, `DM_DATABASE_MIN_CONNECTIONS` | `16`, `1` | The connection pool |
| `DM_DATABASE_ACQUIRE_TIMEOUT_SECONDS` | `5` | Waiting for a pooled connection |
| `DM_DATABASE_STATEMENT_TIMEOUT_SECONDS` | `10` | One statement's limit |
| `DM_LOG_FILTER` | `info,silicon_dm=debug` | Log levels. Whatever the level, DM never logs scripts, audio, transcripts or credentials |

Plain `http` is accepted only for this machine.

### Speech

Speech (voices, spoken messages, listening, transcription) goes through Deepgram from the service only; the key never
reaches a client:

| Variable | Default | Purpose |
| --- | --- | --- |
| `DM_DEEPGRAM_API_KEY` | unset | Deepgram key. Unset: speech and transcription answer 503 `speech_not_configured`, and `GET /api/v1/accounts` says `speech.enabled: false` |
| `DM_DEEPGRAM_URL` | `https://api.deepgram.com` | Deepgram origin for transcription (`/v1/listen`); no path |
| `DM_DEEPGRAM_AGENT_URL` | `wss://agent.deepgram.com/v1/agent/converse` | Deepgram Voice Agent endpoint that renders speech |
| `DM_SPEECH_MODEL` | `eleven_v4` | The ElevenLabs model scripts are spoken with |
| `DM_STT_MODEL` | `nova-3` | The Deepgram transcription model |
| `DM_VOICES` | the five default voices | JSON array of `{"key","name","voice_id","description"}` (1 to 50, unique keys) |
| `DM_SPEECH_CACHE_MAX_BYTES` | `2147483648` (2 GiB) | Rendered audio kept; the least recently used beyond it is evicted |
| `DM_SPEECH_CACHE_TTL_DAYS` | `90` | Rendered audio unused this long is evicted |
| `DM_SPEECH_RENDERS_PER_MINUTE` | `30` | Renders one account may cause per minute (cache hits are free) |
| `DM_SPEECH_DAILY_CHARACTERS` | `200000` | Characters one account may have rendered per UTC day |
| `DM_TRANSCRIBE_MAX_BYTES` | `104857600` (100 MiB) | Largest audio upload a transcription accepts (at most 2 GiB) |
| `DM_TRANSCRIBE_PER_MINUTE` | `20` | Transcriptions one account may request per minute |

Plain `http://` and `ws://` Deepgram URLs are accepted only for this machine (tests and local stand-ins). The audio
cache lives in PostgreSQL (`speech_audio`): it is derived data, never an upload, and any row can be evicted and
rendered again. [Voices and speech](../learn/voices-and-speech.md) explains the choices.

The default voices (`DM_VOICES` unset):

| Key | Name | ElevenLabs voice id |
| --- | --- | --- |
| `mark` | Mark | `UgBBYS2sOqTuMpoF3BR0` |
| `brittney` | Brittney | `kPzsL2i3teMYv0FxEYQ6` |
| `jamal` | Young Jamal | `6OzrBCQf8cjERkYgzSg8` |
| `jessa` | Jessa | `yj30vwTGJxSHezdAGsv9` |
| `hope` | Hope | `tnSpp4vdxKPjI9w0GnoV` |

Changing the catalog changes the default voice of accounts that never chose one (the default is an index into the
list); a stored choice whose key is no longer listed falls back to the default. Spoken messages keep the voice they
were sent in either way.

### Delivery through Ting, presence and GIFs

| Variable | Default | Purpose |
| --- | --- | --- |
| `DM_TING_URL` | unset | Ting delivery; unset means clients sync with `GET /api/v1/sync` |
| `DM_TING_APP_ID` | `ting` | Ting's app id in Silicon Accounts: the receiving app of the proofs DM sends Ting |
| `DM_TING_REQUEST_TIMEOUT_SECONDS` | `15` | Calls to Ting |
| `DM_DELIVERY_BATCH_SIZE` | `100` | Ting handoffs claimed at once |
| `DM_DELIVERY_POLL_INTERVAL_MILLISECONDS` | `250` | How often the handoff worker looks for work when idle |
| `DM_DELIVERY_LEASE_SECONDS` | `30` | How long a claimed handoff is held before another worker may take it |
| `DM_DELIVERY_MAX_ATTEMPTS` | `20` | Attempts before a handoff is set aside |
| `DM_DELIVERY_MAX_RETRY_SECONDS` | `300` | Longest pause between attempts |
| `DM_HEARTBEAT_INTERVAL_SECONDS`, `DM_HEARTBEAT_TIMEOUT_SECONDS` | `30`, `120` | A device's presence lease: clients renew every 30 seconds, and a lease not renewed for 120 seconds ends (both fixed) |
| `DM_PRESENCE_ACTIVITY_TTL_SECONDS` | `10` | How long an activity (typing…) shows without being renewed |
| `DM_WS_OUTBOUND_CAPACITY` | `256` | Kept for older configurations; DM serves no WebSockets |
| `DM_GIPHY_API_KEY` | required | Giphy key for trending and search |
| `DM_GIPHY_API_BASE_URL` | `https://api.giphy.com/v1/gifs` | Giphy's API |
| `DM_PROVIDER_REQUEST_TIMEOUT_SECONDS` | `10` | Calls to Giphy |
| `DM_TRENDING_GIF_CACHE_SECONDS` | `300` | How long trending GIFs are cached |

Attachments are external links; no upload credentials are required by the service.

### Bug report notifications

Set `DM_POSTMARK_SERVER_TOKEN` on the API service to send report notifications. The default `DM_POSTMARK_EMAIL_URL` is
`https://api.postmarkapp.com/email`. Verify `dm@teamofsilicons.com` as a Postmark sender. Reports notify
`saketdev12@gmail.com`, `shubhastro2@gmail.com` and `bugs@teamofsilicons.com`.
Each account may submit `DM_REPORTS_PER_HOUR` new reports an hour (default 10, at most 100000); the next one is 429
`rate_limited`, whose `Retry-After` says when one fits again. Retries with the same idempotency key do not create
another report.

The API returns `202` once the report is stored. Its worker retries Postmark failures with backoff, up to an hour
between attempts. A process crash after Postmark accepts but before the transaction commits can deliver a duplicate
notification; the report id identifies it. Outside production, without a Postmark token, reports are stored and marked
`simulated`. No mail token is distributed in the client or the command.

### Settings DM no longer reads

The settings of the sign-in and installer services DM used before Silicon Accounts and Silicon Apps, of the retired
test environments and of telemetry are ignored. The service names any that are still set once at start, so an
operator can remove them.

## The website

The website's server (Next.js) reads these at request time; none of them reaches the browser:

| Variable | Purpose |
| --- | --- |
| `APP_ID` | `dm` |
| `APP_SECRET` | DM's app secret at Silicon Accounts (the server-side sign-in) |
| `ACCOUNTS_URL`, `ACCOUNTS_API_URL` | Silicon Accounts, as for the service |
| `APP_API_URL` | The DM service the website calls through its same-origin proxy |
| `SESSION_SECRET` | At least 32 random bytes; seals the session cookie (changing it signs every browser out) |
| `PUBLIC_URL` | The website's own origin; `{PUBLIC_URL}/auth/callback` is registered at Silicon Accounts |
| `EXTRA_IMG_ORIGINS`, `EXTRA_ORIGINS` | More trusted profile-photo origins; another explicitly trusted website origin |
| `BRIEFCASE_API_URL` | Silicon Briefcase's API origin, where the website saves a Carbon's files and voice notes (in `apps/dm/` of the Carbon's own Briefcase). Unset: uploads are off and the website says so; links and GIFs still work |
| `UPLOADS_MAX_BYTES` | Optional: a limit of the website's own on one file, in bytes, for an operator who wants one. Unset (the default), the website takes files of any size up to Silicon Briefcase's 5 TiB |
| `KEEP_ALIVE_TIMEOUT` | Optional, in milliseconds (Next's own setting): how long the website keeps an idle connection. Unset, it keeps one 185 seconds, longer than a load balancer keeps its own (production's: 180); behind a balancer whose idle timeout is longer, set it above that, or the balancer now and then reuses a connection the website is closing and answers 502 |

The DM service never handles files: uploads are the website's job, done as DM acting for the Carbon at Silicon
Briefcase with User verification proofs. The website's server passes each file's bytes through to Briefcase as they
arrive and keeps none of them, so it needs no disk for uploads and its memory does not grow with a file's size; what
sits in front of it must not limit or buffer the upload's body ([the website's ingress](../operations/deployment.md#the-website)).
Production needs Briefcase to accept DM's proofs for its upload scopes
([the Briefcase side](../operations/deployment.md#uploads-the-briefcase-side)). `web/README.md` in the repository
covers running and deploying the website.

### When uploads are off

When uploads cannot work, the website tells Carbons only that, in their words: `GET /api/uploads/config` answers
`{"enabled": false, "reason": …}`, the + menu says the reason under its switched-off Upload a file and offers links and
GIFs instead, a file pasted or dropped is answered with the same reason (a dropped link still attaches), `/upload` and
⌘⇧U say it too, and the microphone offers dictation only.
Which setting is wrong is the operator's business, so the website's server writes it to its log, once per process, as
a line that starts `dm: Uploads are off:`. What each one means:

| Carbons read | The server logs | Fix |
| --- | --- | --- |
| "Uploads are not set up on this DM website yet." | `BRIEFCASE_API_URL is not set` | Set `BRIEFCASE_API_URL` to Briefcase's API origin, such as `https://api.briefcase.teamofsilicons.com` (after Briefcase accepts DM's proofs) |
| "Uploads are not working on this DM website: its setup needs fixing." | `BRIEFCASE_API_URL is not a URL`, `… must start with https://`, `… must be an origin (and a path) without credentials, a query or a #fragment` | Give the origin alone; plain `http` works only for a stand-in on this machine |
| the same | `UPLOADS_MAX_BYTES is not a whole number of bytes` | A whole number of bytes, such as `1073741824` (1 GiB), or unset for no limit of the website's own |

Once uploads are on, a refusal by Silicon Accounts or Briefcase reaches the Carbon as the upload's own error
([Errors](errors.md#from-the-website)). 503 `briefcase_not_allowed` means Briefcase does not list `dm` in its
`BRIEFCASE_PROOF_ISSUERS` for a scope an upload needs: before anything is saved, or after the file is saved when only
`briefcase.link_access.update` is missing (the Carbon then reads that the file was saved but its link could not be
opened, and where the copy is). 503 `uploads_misconfigured` means the website's `APP_ID` or `APP_SECRET` is wrong. For
these, and for every other failure that points at Briefcase or Silicon Accounts (a 5xx), the website's server logs one
line with Briefcase's own code, never a file's name: `upload failed: briefcase_not_allowed (proof_scope_missing)`. A
cancel Briefcase refuses is logged as `upload: Silicon Briefcase did not cancel an unfinished upload (…)`: that upload
stays pending there until it expires, within a day.
