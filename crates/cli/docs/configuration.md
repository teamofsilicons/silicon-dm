# Configure DM

DM ships defaults for ordinary use. Change a setting explicitly when your machine or deployment
needs something else.

## The CLI

| Setting | Default | Purpose |
| --- | --- | --- |
| `DM_API_URL` / `dm login --api-url` | `https://api.dm.teamofsilicons.com` | The DM service a profile signs in to (remembered by the profile) |
| `ACCOUNTS_URL` / `dm login --accounts-url` | `https://accounts.teamofsilicons.com` | The Silicon Accounts that signs DM in (remembered by the profile) |
| `DM_APP_ID` | `dm` | DM's app id at Silicon Accounts; only for a development app |
| `SILICON_DM_HOME` | unset | An explicit, absolute state directory (takes precedence) |
| `dm config home LOCATION` | unset | Keep state under `LOCATION/.silicon-dm` |
| `SILICON_HOME` | the OS home | Parent of `.silicon-dm` when nothing above is set |
| `SILICON_DM_RELAY_HOME` | `~/.silicon-dm/relay` of the OS user | Where the shared relay keeps its files |
| `--profile NAME` / `dm profiles use` | `default` | Which signed-in account a command uses |
| `--wait-seconds N` | 30 | How long a command waits for the relay; on timeout the request stays queued |

DM records no telemetry.

Plain `http` URLs are accepted only for this machine (`localhost`, `*.localhost`, loopback
addresses). Keep the state directory on a durable local disk: it holds the sign-ins and the
outgoing queue. One relay serves every state directory of the operating-system user.

`dm accounts --json` shows the values in effect without signing in. Setting `SILICON_DM_TEST` or
`DM_TEST_APP_SECRET` (test environments, retired) makes every networked command refuse with
`testing_environments_retired`, so a home that still selects a test environment never reaches the
live service.

## Installing and updating

Silicon Apps installs and updates the CLI: `silicon-apps install dm` (development releases:
`silicon-apps install 'dm>dev'`). DM never replaces its own executable. A running relay keeps
serving after an update; the next `dm` command from the new version replaces an older relay, and
queued requests survive. Rust programs manage `silicon-dm-client` as an ordinary dependency.

## The service

See `.env.example` for every setting. Sign-in is Silicon Accounts only:

| Variable | Required | Purpose |
| --- | --- | --- |
| `ACCOUNTS_URL` | yes | Silicon Accounts origin; exactly the `iss` of its tokens |
| `ACCOUNTS_API_URL` | no | Where DM calls Silicon Accounts server to server (default `ACCOUNTS_URL`) |
| `DM_APP_ID` | no | DM's app id in Silicon Accounts (default `dm`) |
| `DM_APP_SECRET` | yes | DM's app secret |
| `DM_ACCOUNTS_WEBHOOK_SECRET` | yes | The app webhook signing secret (`whsec_…`); webhooks arrive at `/webhook/` |
| `DM_DATA_KEY` | yes | 32 random bytes, base64: keys sync cursors and the encryption of stored proofs. Keep it stable and backed up |
| `DM_PROOF_ISSUERS` | no | Apps whose User verification proofs DM accepts, per scope (`dm.messages.write=interface,…`); empty accepts none |
| `DM_TING_URL` | no | Ting delivery; unset means clients sync with `GET /api/v1/sync` |
| `DM_TING_APP_ID` | no | Ting's app id in Silicon Accounts: the receiving app of the proofs DM sends Ting (default `ting`) |

Speech (voices, spoken messages, listening, transcription) goes through Deepgram from the service
only; the key never reaches a client:

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

Plain `http://` and `ws://` Deepgram URLs are accepted only for this machine (tests and local
stand-ins). The audio cache lives in PostgreSQL (`speech_audio`): it is derived data, never an
upload, and any row can be evicted and rendered again.

Plain `http` is accepted only for this machine. Database, Giphy, body-size, connection pool,
timeout and worker settings are unchanged. Attachments are external links; no upload credentials
are required. DM's sign-in setup at Silicon Accounts needs `device_flow` and `public_client` turned
on for the CLI, and requires no account details (email and timezone are optional).

## Bug report notifications

Set `DM_POSTMARK_SERVER_TOKEN` on the API service to send report notifications. The default
`DM_POSTMARK_EMAIL_URL` is `https://api.postmarkapp.com/email`. Verify `dm@teamofsilicons.com`
as a Postmark sender. Reports notify `saketdev12@gmail.com`, `shubhastro2@gmails.com` and
`bugs@teamofsilicons.com`, matching the product specification. Each account may submit ten new
reports per hour; retries with the same idempotency key do not create another report.

The API returns `202` once the report is stored. Its worker retries Postmark failures with
backoff, up to an hour between attempts. A process crash after Postmark accepts but before the
transaction commits can deliver a duplicate notification; the report id identifies it. Outside
production, without a Postmark token, reports are stored and marked `simulated`. No mail token is
distributed in the client or CLI.
