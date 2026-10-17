# The local chat stack

Everything DM needs to run end to end on this machine, behind one command, so the DM service, the CLI and the
website can be built and tested against real sign-ins, real tokens and real delivery, without touching a deployed
service. All state lives in `.dev/` at the worktree root (git-ignored); secrets are in `0600` files there and are
never printed unless a command was asked for exactly that secret.

```sh
scripts/chat-stack/up.sh                  # start whatever is not running (idempotent), print the URLs
scripts/chat-stack/up.sh --restart-dm     # rebuild DM, re-run its migrations, restart dm-api (picks up new routes)
scripts/chat-stack/status.sh              # JSON health of every part; exit 1 when a required part is down
node scripts/chat-stack/smoke.mjs         # prove it: CLI send, API read, both Deepgram protocols, GIFs, a Briefcase upload, the website
scripts/chat-stack/down.sh                # stop what up.sh started (--keep-accounts, --db)
```

`.dev/stack.json` (0600) holds every URL, port, app secret, webhook secret and the website's session secret, plus
the exact commands and each identity's CLI environment. Read it instead of hard-coding anything.

## What runs where

| Part | Address | Notes |
|---|---|---|
| Postgres 16 | `127.0.0.1:5500` | `.dev/pg`, trust auth, user `postgres`; databases `accounts_dmchat` and `dm_chat`. Never re-initialised; stopped only by `down.sh --db`. |
| Silicon Accounts | site `http://localhost:9840`, API `http://127.0.0.1:9839` | `silicon-accounts/scripts/dev.sh` with its testkit: mock OIDC 9841, mock email/SMS 9842 (`/_messages`), fake apps 9843, mock Iris 9844, Apps stand-in 9846. The site's URL is the token issuer. |
| dm-api | `http://127.0.0.1:4521` | Started by `scripts/dev_accounts.py` from binaries snapshotted into `.dev/bin`; migrated as the owner, runs as `dm_chat_runtime` with `deploy/runtime-grants.sql`; DM's Accounts webhook points here. Log: `.dev/dm/logs/dev-api.log`. |
| Deepgram stand-in | `http://127.0.0.1:4540` | `POST /v1/listen` and `ws://…/v1/agent/converse`. See below. |
| Briefcase stand-in | `http://127.0.0.1:4541` | Delegated uploads and public links. See below. |
| Giphy stand-in | `http://127.0.0.1:4543` | dm-api's GIF trending and search (`DM_GIPHY_API_BASE_URL`): six popular GIFs with Giphy's own https media URLs; an unknown word such as `zzz` finds nothing. |
| Ting stand-in | `http://127.0.0.1:4522` | Only with `up.sh --ting` (`scripts/dev_ting_stub.py`, as the app `ting`). It predates DM's current Ting contract (DM sends its proofs as `Bearer`, the stand-in reads `Proof`), so enrolment is refused there; DM's sync feed is the default transport and needs no Ting. |
| dm relay | `127.0.0.1:4542` | The CLI's shared relay for every identity home, in `.dev/relay` (never `~/.silicon-dm/relay`). |
| Website | dev `4520`, Playwright build `4530` | `up.sh` writes `web/.env.local`; `up.sh --web` also runs `next dev` on 4520. |

At Silicon Accounts, DM's sign-in setup lists `http://127.0.0.1:4520/auth/callback` and `http://127.0.0.1:4530/auth/callback`
(and the `localhost` forms), with the device flow and public clients on and no required details; `up.sh` re-applies
it every run. The app `ting` is added once. The apps `dm`, `interface` and `briefcase` use the testkit's fixed
development secrets.

## Identities

`up.sh` creates the pool once (`.dev/identities.json`, 0600): four Carbons and six Silicons.

| Carbon | Looks after | Silicon allow-lists in DM |
|---|---|---|
| `ada` (`c:dmc-ada`) | `scout`, `quill` | scout: bo, cy, atlas, ember. quill: bo, cy |
| `bo` (`c:dmc-bo`) | `atlas`, `pip` | atlas: ada, cy, scout, ember. pip: ada, cy |
| `cy` (`c:dmc-cy`) | `ember`, `nova` | ember: ada, bo, scout, atlas. nova: ada, bo |
| `di` (`c:dmc-di`) | nobody | on nobody's list: use Di to see a refusal |

Silicon ids are `si:dmc-<name>`. A custodian and its other Silicons never need listing; every `up.sh` puts these
allow-lists back. Name anyone by short name, id (`c:dmc-ada`, also `c:ada`) or uuid:

```sh
H=scripts/chat-stack/identities.mjs
TOKEN=$(node $H token ada --raw)          # a DM access token (a new sign-in each call; --raw prints only the token)
node $H slt scout | dm login --slt-stdin  # a short-lived token for the CLI (single use, 2 minutes)
node $H login scout                       # sign the CLI in, in scout's own SILICON_HOME (a Carbon approves its device code automatically)
node $H dm scout -- messages send c:dmc-ada --text 'hi' --wait   # run the CLI as scout (DM_BIN=<path> for another build)
eval "$(node $H env scout)"               # SILICON_HOME, SILICON_DM_RELAY_HOME, DM_API_URL, ACCOUNTS_URL, PATH for scout
node $H cookie ada                        # a sealed website session cookie (JSON for Playwright; --header for curl)
node $H storage-state ada --out test-results/.auth/ada.json   # Playwright storage state, incl. Ada's Accounts session
node $H proof ada --scope briefcase.uploads.reserve,briefcase.uploads.commit   # a User verification proof, dm → briefcase
node $H init-demo                         # the demo workspace's own accounts: mira (looks after sol and wren); see below
```

No email code is spent after the pool exists: a Carbon keeps its account-site session (`sa_session`, 900 days,
never rotates) and every token, short-lived token, Silicon or device approval is made from it; a Silicon signs in
with its STK. Each `token` and `cookie` is a new sign-in with its own refresh token, so parallel tests never trip
refresh-token reuse detection. The website cookie works on 4520 and 4530 because both read `SESSION_SECRET` from
`web/.env.local` (Playwright's `E2E_SESSION_SECRET` falls back to it).

## The demo workspace and its screenshots

Tests fill the pool's lists with groups named after their runs, so DM is never looked at as Ada or Bo for a design
review. The demo workspace is DM on an ordinary morning instead: Mira Patel (`c:mira`) leads the launch of Juniper, a
calendar for small studios; Sol (`si:sol`) and Wren (`si:wren`) are the Silicons she looks after (their voices Mark
and Hope, chosen by her); Bo (`c:dmc-bo`, from the pool) designs it with her. No test signs in as Mira.

```sh
node scripts/chat-stack/seed-demo.mjs             # make what is missing, keep what is there; prints what it made (JSON)
node scripts/chat-stack/seed-demo.mjs --check     # what exists and what is missing, changing nothing (exit 1 if anything is)
node scripts/chat-stack/seed-demo.mjs --fresh     # a new set of accounts (c:mira-2, si:sol-2, …): every conversation starts empty
node scripts/chat-stack/seed-demo.mjs --set 1     # back to an earlier set, its accounts and conversations as they were
node scripts/chat-stack/identities.mjs cookie mira   # sign a browser in as Mira (every identities.mjs command knows mira, sol, wren)
node scripts/chat-stack/screens.mjs --base http://127.0.0.1:4530 --out /tmp/dm-screens   # every surface, 1440 and 390 px
node scripts/chat-stack/screens.mjs --list        # the surfaces; --only a,b and --skip a,b choose some
```

What the seed makes, through DM's API and CLI (the Silicons speak with `dm messages send --speak`, signed in in their
own homes):

| Conversation | What is in it |
| --- | --- |
| Mira and Sol | The morning summary as a spoken message with cues and a caption; a PDF; a code block; Mira's reply that quotes the PDF and was edited (a typo, so it has history); a voice note from Mira with its transcript; a short spoken message; Read and Delivered |
| Mira and Wren | A few messages, one spoken, the last one unread |
| Launch week (`g:launch-week`) | Mira, Bo, Sol and Wren: replies, Bo's two hero mock-ups, Wren's chart, a whiteboard photo, Sol's checklist bundled under one message, a message Wren routes to Sol's `release-notes` intent, Bo's GIF; the last two unread |
| Mira and Bo | A short Carbon chat (the screenshots add Bo typing) |
| Sol and Wren | Their own conversation, each message routed to an intent: what Mira sees when she views as Sol |
| Design crit | Bo's invitation, waiting for Mira |

The pictures and the PDF are small HTML pages rendered by the website's Playwright (`lib/demo-assets.mjs`, kept in
`.dev/demo/assets`) and uploaded into the sender's own Briefcase (the stand-in) with User verification proofs, the
same requests `web/lib/server/briefcase.ts` makes for the website, so the website shows them through its file proxy.
Mira's voice note is her own voice rendered by `POST /api/v1/speech`. The seed records each step's message code and
creation time in `.dev/demo.json` and checks them against the service on every run: a run after a crash goes on where
it stopped, a run after `up.sh --fresh-dm` makes everything again. DM cannot backdate messages, so every message has the
time of the run; the screenshot harness picks a time zone where that time is the morning (`--timezone` to choose).
The demo accounts live under `demo` in `.dev/identities.json`, outside the pool's ten (which `status.sh` counts and
`init` rewrites); `identities.mjs init-demo` makes and checks them (`up.sh` keeps them but does not check them).

`screens.mjs` signs Chromium in as Mira (a new sign-in per width) with a fake microphone and writes
`<surface>-<width>.png` and an `index.md` that names what each one shows. It runs against any website (`--base`): the
dev site, or a production build (`next build` into your own `NEXT_DIST_DIR`, then `next start` with the environment
`web/playwright.config.ts` gives it). It leaves the workspace as it found it: it never marks anything read (the
website's receipts are held in its browser), clears what it typed, discards the send it fails on purpose and cancels
its recordings and uploads. Listen mode needs a message to arrive, so Wren sends one line per run (every width listens
at once), and at the end everything older in Wren's conversation is marked read: the inbox keeps one unread message
there. Sol, Wren and Bo hold presence leases while it runs (they show online), and Bo types while his conversation is
taken. The pointer rests in a corner for every picture, so no hover toolbar shows unless the surface is about it.

## Speech

dm-api gets `DM_DEEPGRAM_API_KEY` (any value), `DM_DEEPGRAM_URL=http://127.0.0.1:4540` and
`DM_DEEPGRAM_AGENT_URL=ws://127.0.0.1:4540/v1/agent/converse`. With `DM_DEEPGRAM_REAL=1 scripts/chat-stack/up.sh`
it talks to the real Deepgram instead, with `WAVEFORM_DEEPGRAM_API_KEY` read from `silicon-waveform/.env` at start
into dm-api's environment only (never printed, never written, not even to `.dev`). A plain `up.sh` switches back.
More dm-api settings (non-secret, e.g. `DM_SPEECH_RENDERS_PER_MINUTE=1000`) go in `.dev/dm-api.env` as `KEY=VALUE`
lines; the next `up.sh` restarts dm-api with them.

The **Deepgram stand-in** answers the Voice Agent protocol (Welcome, Settings → SettingsApplied, InjectAgentMessage →
ConversationText + linear16 PCM of about 65 ms per spoken character + AgentAudioDone, KeepAlive) with a soft voiced
tone that follows the words. Cues behave as the real ElevenLabs v4 did when measured: they are not read aloud, pause
cues add silence (`[short pause]` 0.8 s, `[long pause]` 1.7 s), reaction cues (`[laughs]`, `[sighs]`…) add a short
sound, and a script of cues only fails. It answers `INVALID_SETTINGS` for an audio setup ElevenLabs cannot deliver and
`FAILED_TO_SPEAK` for a voice outside the five. `POST /v1/listen` answers Deepgram's JSON with "This is a voice note
from the stand-in." (plus its length from 4 s on; empty for silence) and the real duration of WAV, WebM (also a
MediaRecorder recording without a duration), Ogg, MP4/M4A, FLAC, MP3 and AAC audio. Provoke failures with
`POST /_faults` (for example `{"target":"agent","code":"FAILED_TO_SPEAK"}`, `{"target":"agent","mode":"hang"}` or
`{"target":"listen","status":503}`); `GET /_requests` shows what it was asked. It logs to `.dev/logs/deepgram.log`,
never bodies, text or keys.

## Uploads

The **Briefcase stand-in** implements Briefcase 4's delegated uploads with the same shapes and error codes:
`POST /api/v1/obo/uploads/{reserve,commit,status,cancel}`, the byte transfer `PUT /api/v1/obo/uploads/{upload_id}/content`
(`X-Briefcase-Upload-Capability`, an exact `Content-Length`, no chunked encoding, no `Authorization`),
`POST /api/v1/obo/folders/create`, `POST /api/v1/obo/link-access` and `GET /api/v1/public/{owner}/{path}?view=inline|attachment|metadata|contents`.
Every proof is verified with the local Accounts stack using briefcase's credentials and must carry the route's
scope. Like Briefcase, public bytes come with `Cross-Origin-Resource-Policy: same-origin` and a sandbox CSP, so the
website must serve them through its own origin rather than embed them directly. Files live in `.dev/briefcase/`;
`GET /_state` lists them.

## When something is wrong

The ports 4600–4649 (service tests), 4650–4699 (CLI) and 4700–4749 (website) are left free for other test runs.
`status.sh` names the part that is down. Logs: `.dev/logs/` (stand-ins, the website, builds), dm-api in
`.dev/dm/logs/dev-api.log`, Silicon Accounts in `silicon-accounts/.dev/logs/9840/`. `up.sh` repairs what it can on
every run (it restarts a stand-in whose code changed, re-points DM's webhook, re-checks every identity). If
`--restart-dm` cannot build, dm-api keeps running the previous build and the error names the log.
