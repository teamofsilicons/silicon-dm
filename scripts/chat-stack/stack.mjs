#!/usr/bin/env node
// The local chat stack (spec §9): one entry point for everything DM needs on this machine.
//
//   scripts/chat-stack/up.sh [options]       start whatever is not running (idempotent), print the URLs
//   scripts/chat-stack/down.sh [options]     stop what up.sh started
//   scripts/chat-stack/status.sh             JSON health of every part (exit 1 when a required part is down)
//
// up.sh options:
//   --restart-dm        rebuild DM (cargo build -p silicon-dm --bins -p silicon-dm-cli into the shared target), snapshot
//                       the binaries into .dev/bin, re-run the migrations and restart dm-api: picks up new routes
//   --no-build          with --restart-dm: snapshot the binaries already in the shared target, without building
//   --ting              deliver through the Ting stand-in (scripts/dev_ting_stub.py on 4522, as the app `ting`)
//   --web               also run the dev website (pnpm dev) on 4520 when nothing listens there
//   --accounts-web=M    how the Accounts stack serves its public origin: next (default), proxy or none
//   --fresh-dm          drop and recreate DM's database dm_chat first (all DM data is lost)
//   --reset-accounts    drop and recreate the Accounts database first; DM's database, the identity pool, the CLI homes
//                       and the relay's queues go with it and are made again (the pool costs 4 email codes)
//   --skip-identities   do not create or check the identity pool
//   DM_DEEPGRAM_REAL=1  dm-api talks to the real Deepgram with the key from silicon-waveform/.env (read at start,
//                       handed to dm-api's environment only, never printed or written)
// down.sh options:
//   --keep-accounts     leave the Silicon Accounts stack running (it takes a while to start)
//   --db                also stop the Postgres cluster on 5500 (never stopped otherwise)
import { spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { existsSync, readFileSync, renameSync, rmSync, statSync } from 'node:fs';
import { join } from 'node:path';
import {
  ACCOUNTS_CHECKOUT, GIT, HERE, PATHS, PG_BIN, PORTS, ROOT, SHARED_TARGET, StackError, WAVEFORM_ENV, describe, ensureDir,
  freeGiB, http, listener, livePid, parseDotenv, portOpen, randomSecret, readJson, sleep, startDetached,
  stopRecorded, withLock, writeJsonPrivate, writePrivate,
} from './lib/common.mjs';

const argv = process.argv.slice(2);
const command = argv[0];
const flag = (name) => argv.includes(`--${name}`);
const valued = (name, fallback) => {
  const found = argv.find((arg) => arg.startsWith(`--${name}=`));
  return found ? found.slice(name.length + 3) : fallback;
};
const say = (text) => process.stderr.write(`chat-stack: ${text}\n`);

const URLS = {
  accountsSite: `http://localhost:${PORTS.accounts_site}`,
  accountsSiteLoopback: `http://127.0.0.1:${PORTS.accounts_site}`,
  accountsApi: `http://127.0.0.1:${PORTS.accounts_api}`,
  mockOidc: `http://127.0.0.1:${PORTS.mock_oidc}`,
  mockMessaging: `http://127.0.0.1:${PORTS.mock_messaging}`,
  fakeApps: `http://127.0.0.1:${PORTS.fake_apps}`,
  mockIris: `http://127.0.0.1:${PORTS.mock_iris}`,
  appsStandIn: `http://127.0.0.1:${PORTS.apps_stand_in}`,
  dmApi: `http://127.0.0.1:${PORTS.dm_api}`,
  ting: `http://127.0.0.1:${PORTS.ting_stand_in}`,
  webDev: `http://127.0.0.1:${PORTS.web_dev}`,
  webE2e: `http://127.0.0.1:${PORTS.web_e2e}`,
  deepgram: `http://127.0.0.1:${PORTS.deepgram}`,
  deepgramAgent: `ws://127.0.0.1:${PORTS.deepgram}/v1/agent/converse`,
  briefcase: `http://127.0.0.1:${PORTS.briefcase}`,
  giphy: `http://127.0.0.1:${PORTS.giphy}`,
};
const POSTGRES = `postgres://postgres@127.0.0.1:${PORTS.postgres}`;
const DB = { accounts: 'accounts_dmchat', dm: 'dm_chat', dmRuntime: 'dm_chat_runtime' };
const ACCOUNTS_RUN = join(ACCOUNTS_CHECKOUT, '.dev', 'run', String(PORTS.accounts_site));
const ACCOUNTS_LOGS = join(ACCOUNTS_CHECKOUT, '.dev', 'logs', String(PORTS.accounts_site));
const ACCOUNTS_BIN = join(ACCOUNTS_CHECKOUT, 'target', 'integration', 'debug');
/** DM's redirect URIs at the stack: both websites, on both loopback names (Accounts ignores a loopback port anyway). */
const DM_REDIRECTS = [PORTS.web_dev, PORTS.web_e2e].flatMap((port) => [`http://127.0.0.1:${port}/auth/callback`, `http://localhost:${port}/auth/callback`]);
const STAND_INS = {
  deepgram: { script: join(HERE, 'deepgram-standin.mjs'), port: PORTS.deepgram, log: join(PATHS.logs, 'deepgram.log') },
  briefcase: { script: join(HERE, 'briefcase-standin.mjs'), port: PORTS.briefcase, log: join(PATHS.logs, 'briefcase.log') },
  giphy: { script: join(HERE, 'giphy-standin.mjs'), port: PORTS.giphy, log: join(PATHS.logs, 'giphy.log') },
};
const PG_OPTIONS = `-p ${PORTS.postgres} -k /tmp -c listen_addresses=127.0.0.1 -c max_connections=300 -c shared_buffers=128MB -c fsync=off -c synchronous_commit=off`;

function loadState() {
  return readJson(PATHS.state, { started: {} });
}
function saveState(state) {
  writeJsonPrivate(PATHS.state, { ...state, updated_at: new Date().toISOString() });
}

function run(commandLine, args, options = {}) {
  const done = spawnSync(commandLine, args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, ...options });
  return { code: done.status ?? (done.error ? 127 : 1), stdout: done.stdout ?? '', stderr: done.stderr ?? '', error: done.error };
}

const tail = (text, lines = 12) => text.trim().split('\n').slice(-lines).join('\n');
const sha = (text) => createHash('sha256').update(text).digest('hex').slice(0, 16);

// ------------------------------------------------------------------------------------------------ Postgres

function pgReady() {
  return run(join(PG_BIN, 'pg_isready'), ['-h', '127.0.0.1', '-p', String(PORTS.postgres), '-q']).code === 0;
}

function ensurePostgres(state) {
  if (pgReady()) return 'running';
  if (!existsSync(join(PATHS.pg, 'PG_VERSION'))) {
    throw new StackError(`No Postgres answers on 127.0.0.1:${PORTS.postgres} and ${PATHS.pg} holds no cluster.`, 'The chat stack never initialises a cluster; ask whoever owns this worktree to create .dev/pg (Postgres 16, trust auth, user postgres).');
  }
  const status = run(join(PG_BIN, 'pg_ctl'), ['-D', PATHS.pg, 'status']);
  if (status.code === 0) throw new StackError(`A Postgres server runs from ${PATHS.pg} but does not answer on 127.0.0.1:${PORTS.postgres}.`, `Look at ${PATHS.pgLog}.`);
  say(`starting Postgres from ${PATHS.pg} on 127.0.0.1:${PORTS.postgres}`);
  const started = run(join(PG_BIN, 'pg_ctl'), ['-D', PATHS.pg, '-o', PG_OPTIONS, '-l', PATHS.pgLog, '-w', '-t', '60', 'start']);
  if (started.code !== 0 || !pgReady()) throw new StackError(`Postgres did not start: ${tail(started.stderr || started.stdout)}`, `Look at ${PATHS.pgLog}.`);
  state.started.postgres = true;
  return 'started';
}

function psql(database, sql) {
  const done = run(join(PG_BIN, 'psql'), ['-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1', '-d', `${POSTGRES}/${database}`, '-c', sql]);
  return done.code === 0 ? done.stdout.trim() : null;
}

// ------------------------------------------------------------------------------------------------ Silicon Accounts

async function accountsHealth() {
  const checks = {
    api: await http('GET', `${URLS.accountsApi}/readyz`, { timeoutMs: 3000 }),
    mock_messaging: await http('GET', `${URLS.mockMessaging}/_messages?limit=1`, { timeoutMs: 3000 }),
    fake_apps: await http('GET', `${URLS.fakeApps}/_health`, { timeoutMs: 3000 }),
    mock_oidc: await http('GET', `${URLS.mockOidc}/google/.well-known/openid-configuration`, { timeoutMs: 3000 }),
    mock_iris: await http('GET', `${URLS.mockIris}/pfp/carbon?id=00000000-0000-4000-8000-000000000000`, { timeoutMs: 3000, headers: { accept: 'image/svg+xml' } }),
  };
  const mode = readJson(PATHS.state, { started: {} }).accounts_web ?? 'next';
  if (mode !== 'none') checks.site = await http('GET', `${URLS.accountsSiteLoopback}/v1/meta`, { timeoutMs: 30_000 });
  const parts = Object.fromEntries(Object.entries(checks).map(([name, answer]) => [name, answer.status === 200]));
  return { ok: Object.values(parts).every(Boolean), parts };
}

function fakeApps() {
  const file = join(ACCOUNTS_CHECKOUT, 'testkit', 'fake-apps.json');
  const apps = readJson(file).apps;
  const pick = (id) => apps.find((app) => app.app_id === id) ?? (() => { throw new StackError(`${file} has no app ${id}.`); })();
  return { dm: pick('dm'), interface: pick('interface'), briefcase: pick('briefcase') };
}

/** The `ting` app this stack adds to the Accounts stack (its secret is generated once and kept in .dev/accounts). */
function tingApp() {
  const existing = readJson(PATHS.accountsApps, null);
  const kept = existing?.apps?.find((app) => app.app_id === 'ting');
  if (kept) return kept;
  // Silicon Apps' secret shape: sa_app_<app_id>_ and 40 letters or digits.
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  const secret = `sa_app_ting_${Array.from(randomBytes(40), (byte) => alphabet[byte % alphabet.length]).join('')}`;
  const app = {
    app_id: 'ting', name: 'Silicon Ting', description: 'Every signal, in sync.', homepage_url: `${URLS.ting}/`,
    owner_id: 'c:saket', owner_email: 'saketdev12@example.test', secret, status: 'active', created_at: '2026-10-17T00:00:00.000Z',
    signin_defaults: {
      methods: { email: true, phone: false, google: false, apple: false }, method_order: ['email'], required_fields: [], optional_fields: [],
      allow_signup: true, remember_browser: true, copy: { title: 'Sign in to Silicon Ting', subtitle: 'Every signal, in sync.' },
      redirect_uris: [`${URLS.ting}/auth/callback`], allowed_origins: [URLS.ting], allowed_email_domains: [], device_flow: true, public_client: true,
    },
    testkit: { category: 'delivery', purpose: 'Ting for the DM chat stack.', exercises: [], integration: 'hosted', silicon_slt: true, accent: '#1F5FB8', authorize_params: {},
      proofs: { user_verification_issuer_to: [], user_verification_receiver: true, app_verification_issuer_to: [], app_verification_receiver: true } },
  };
  writeJsonPrivate(PATHS.accountsApps, { apps: [app] });
  return app;
}

async function ensureAccounts(state) {
  const webMode = valued('accounts-web', state.accounts_web ?? 'next');
  if (!['next', 'proxy', 'none'].includes(webMode)) throw new StackError(`--accounts-web=${webMode} is not a mode.`, 'Use next (the account site, default), proxy (API behind a small proxy, no pages) or none (API only).');
  const reset = flag('reset-accounts');
  if (!reset && webMode === (state.accounts_web ?? 'next')) {
    const health = await accountsHealth();
    if (health.ok) return 'running';
    say(`the Accounts stack is not fully up (${Object.entries(health.parts).filter(([, ok]) => !ok).map(([name]) => name).join(', ')} down): starting it`);
  }
  for (const binary of ['accounts-api', 'accounts-migrate', 'accounts-seed', 'silicon-accounts']) {
    if (!existsSync(join(ACCOUNTS_BIN, binary))) throw new StackError(`${join(ACCOUNTS_BIN, binary)} does not exist.`, `Build it in ${ACCOUNTS_CHECKOUT}: CARGO_TARGET_DIR=target/integration cargo build -p silicon-accounts-server -p silicon-accounts-cli (the chat stack never builds Accounts itself).`);
  }
  const env = {
    PATH: process.env.PATH, HOME: process.env.HOME, TMPDIR: process.env.TMPDIR ?? '/tmp', LANG: process.env.LANG ?? 'en_US.UTF-8',
    ACCOUNTS_PORT: String(PORTS.accounts_site), ACCOUNTS_API_PORT: String(PORTS.accounts_api), MOCK_OIDC_PORT: String(PORTS.mock_oidc),
    MOCK_MESSAGING_PORT: String(PORTS.mock_messaging), FAKE_APPS_PORT: String(PORTS.fake_apps), MOCK_IRIS_PORT: String(PORTS.mock_iris),
    DEVELOPER_PORT: String(PORTS.accounts_developer), APPS_API_PORT: String(PORTS.apps_stand_in), ACCOUNTS_PGPORT: String(PORTS.postgres),
    ACCOUNTS_PGDATA: PATHS.pg, ACCOUNTS_DB_NAME: DB.accounts, CARGO_TARGET_DIR: 'target/integration', NEXT_TELEMETRY_DISABLED: '1',
  };
  const args = ['--detach', '--developer=off', '--no-build', `--web=${webMode}`, ...(reset ? ['--reset-db'] : [])];
  say(`starting Silicon Accounts (${join(ACCOUNTS_CHECKOUT, 'scripts', 'dev.sh')} ${args.join(' ')}) on ${PORTS.accounts_site}/${PORTS.accounts_api}`);
  const done = run('bash', [join(ACCOUNTS_CHECKOUT, 'scripts', 'dev.sh'), ...args], { cwd: ACCOUNTS_CHECKOUT, env, timeout: 15 * 60_000 });
  writePrivate(join(PATHS.logs, 'accounts-up.log'), `${done.stdout}\n${done.stderr}`);
  if (done.code !== 0) throw new StackError(`Silicon Accounts did not start: ${tail(done.stderr || done.stdout)}`, `Full output: ${join(PATHS.logs, 'accounts-up.log')}; its logs: ${ACCOUNTS_LOGS}/.`);
  state.started.accounts = true;
  state.accounts_web = webMode;
  saveState(state);
  for (let i = 0; i < 120; i++) {
    const health = await accountsHealth();
    if (health.ok) return 'started';
    await sleep(1000);
  }
  throw new StackError('Silicon Accounts started but does not answer every check after 2 minutes.', `Its logs: ${ACCOUNTS_LOGS}/.`);
}

/** Adds the ting app (once) and gives DM the sign-in setup the website and the CLI need. */
async function configureAccounts(apps) {
  const ting = tingApp();
  const tingAnswer = await http('GET', `${URLS.accountsApi}/v1/apps/ting`, { basic: ['ting', ting.secret] });
  if (tingAnswer.status !== 200) {
    const envFile = join(ACCOUNTS_RUN, 'accounts-api.env');
    const seeded = run('bash', ['-c', 'set -a; . "$1"; set +a; exec "$2" --fake-apps "$3"', 'seed', envFile, join(ACCOUNTS_BIN, 'accounts-seed'), PATHS.accountsApps], { cwd: ACCOUNTS_CHECKOUT });
    if (seeded.code !== 0) throw new StackError(`Adding the ting app to the Accounts stack failed: ${tail(seeded.stderr || seeded.stdout)}`);
    say('added the app ting to the Accounts stack');
  }
  const patch = {
    required_fields: [], optional_fields: ['email', 'timezone'], flow: null,
    methods: { email: true, phone: true }, method_order: ['email', 'phone', 'google', 'apple'],
    redirect_uris: [...DM_REDIRECTS, `${URLS.fakeApps}/dm/callback`],
    allowed_origins: [PORTS.web_dev, PORTS.web_e2e].flatMap((port) => [`http://127.0.0.1:${port}`, `http://localhost:${port}`]).concat(URLS.fakeApps),
    device_flow: true, public_client: true,
  };
  const before = await http('GET', `${URLS.accountsApi}/v1/apps/dm`, { basic: ['dm', apps.dm.secret] });
  if (before.status !== 200) throw new StackError(`Reading DM's app at the Accounts stack failed: ${describe(before)}`);
  const answer = await http('PATCH', `${URLS.accountsApi}/v1/apps/dm/signin-config`, {
    basic: ['dm', apps.dm.secret], headers: { 'idempotency-key': `chat-stack-${sha(JSON.stringify(patch))}-${before.json.config_version}` }, json: patch,
  });
  if (answer.status !== 200) throw new StackError(`Giving DM its sign-in setup at the Accounts stack failed: ${describe(answer)}`);
  if (answer.json.config_version !== before.json.config_version) say(`DM's sign-in setup at the Accounts stack is now version ${answer.json.config_version} (redirects for ${PORTS.web_dev} and ${PORTS.web_e2e}, device flow, public client, no required details)`);
  return { ting };
}

// ------------------------------------------------------------------------------------------------ secrets and files

function secrets() {
  const kept = readJson(PATHS.secrets, {});
  let changed = false;
  if (!kept.website_session_secret) {
    kept.website_session_secret = randomSecret(48);
    changed = true;
  }
  if (changed) writeJsonPrivate(PATHS.secrets, kept);
  return kept;
}

function buildInfo() {
  return readJson(PATHS.build, null);
}

function speechMode() {
  return process.env.DM_DEEPGRAM_REAL === '1' ? 'real' : 'stand-in';
}

/** Extra, non-secret settings for dm-api from .dev/dm-api.env (KEY=VALUE lines), e.g. to lower a limit in a test. */
function extraDmEnv() {
  if (!existsSync(PATHS.dmApiEnv)) return {};
  const parsed = parseDotenv(readFileSync(PATHS.dmApiEnv, 'utf8'));
  for (const name of Object.keys(parsed)) {
    if (!/^[A-Z][A-Z0-9_]*$/.test(name)) throw new StackError(`${PATHS.dmApiEnv} sets ${name}; names must look like DM_SPEECH_RENDERS_PER_MINUTE.`);
  }
  return parsed;
}

function composeStack({ apps, ting, secretsKept, state, pool, tingOn }) {
  const build = buildInfo();
  const identities = pool?.identities ?? [];
  const perIdentity = Object.fromEntries(identities.map((identity) => [identity.name, {
    id: identity.id, kind: identity.kind,
    env: { SILICON_HOME: identity.home, SILICON_DM_RELAY_HOME: PATHS.relay, DM_API_URL: URLS.dmApi, ACCOUNTS_URL: URLS.accountsSite },
    exports: `export SILICON_HOME='${identity.home}' SILICON_DM_RELAY_HOME='${PATHS.relay}' DM_API_URL=${URLS.dmApi} ACCOUNTS_URL=${URLS.accountsSite} PATH="${PATHS.bin}:$PATH"`,
  }]));
  const helper = `node ${join(HERE, 'identities.mjs')}`;
  return {
    version: 1,
    generated_at: new Date().toISOString(),
    worktree: ROOT,
    // The keys scripts/dev_accounts.py and tests/e2e read from a stack file (DM_TEST_STACK=<this file>).
    accounts_public_url: URLS.accountsSite,
    accounts_api_url: URLS.accountsApi,
    mock_messaging_url: URLS.mockMessaging,
    apps: {
      dm: { app_secret: apps.dm.secret, webhook_secret_seeded: apps.dm.webhook_secret },
      interface: { app_secret: apps.interface.secret },
      briefcase: { app_secret: apps.briefcase.secret },
      ting: { app_secret: ting.secret },
    },
    ports: PORTS,
    // Free blocks for other agents' own parallel test servers (spec §9); the stack never binds them.
    test_lanes: { service_tests: '4600-4649', cli: '4650-4699', web: '4700-4749' },
    postgres: { url: POSTGRES, data_dir: PATHS.pg, log: PATHS.pgLog, databases: { accounts: DB.accounts, dm: DB.dm }, dm_runtime_role: DB.dmRuntime,
      psql: `${join(PG_BIN, 'psql')} ${POSTGRES}/${DB.dm}` },
    accounts: {
      public_url: URLS.accountsSite, issuer: URLS.accountsSite, api_url: URLS.accountsApi, jwks_url: `${URLS.accountsApi}/.well-known/jwks.json`,
      site_mode: state.accounts_web ?? 'next', mock_oidc_url: URLS.mockOidc, mock_messaging_url: URLS.mockMessaging, fake_apps_url: URLS.fakeApps,
      mock_iris_url: URLS.mockIris, apps_stand_in_url: URLS.appsStandIn, database_url: `${POSTGRES}/${DB.accounts}`, checkout: ACCOUNTS_CHECKOUT,
      logs: ACCOUNTS_LOGS, cli: `${join(ACCOUNTS_BIN, 'silicon-accounts')} --url ${URLS.accountsSite}`,
      start: `cd ${ACCOUNTS_CHECKOUT} && ACCOUNTS_PORT=${PORTS.accounts_site} ACCOUNTS_API_PORT=${PORTS.accounts_api} MOCK_OIDC_PORT=${PORTS.mock_oidc} MOCK_MESSAGING_PORT=${PORTS.mock_messaging} FAKE_APPS_PORT=${PORTS.fake_apps} MOCK_IRIS_PORT=${PORTS.mock_iris} DEVELOPER_PORT=${PORTS.accounts_developer} APPS_API_PORT=${PORTS.apps_stand_in} ACCOUNTS_PGPORT=${PORTS.postgres} ACCOUNTS_PGDATA=${PATHS.pg} ACCOUNTS_DB_NAME=${DB.accounts} CARGO_TARGET_DIR=target/integration scripts/dev.sh --detach --developer=off --no-build`,
      stop: `cd ${ACCOUNTS_CHECKOUT} && ACCOUNTS_PORT=${PORTS.accounts_site} scripts/stop.sh   (never --all or --db)`,
      dm_signin: { redirect_uris: DM_REDIRECTS, device_flow: true, public_client: true, required_fields: [] },
    },
    dm: {
      api_url: URLS.dmApi, live: `${URLS.dmApi}/live`, ready: `${URLS.dmApi}/ready`, api_base_url: `${URLS.dmApi}/api/v1`,
      webhook_url: `${URLS.dmApi}/webhook/`, webhook_secret: readSecretFile(join(PATHS.dmDir, 'dev', 'webhook-secret')) ?? apps.dm.webhook_secret,
      database: DB.dm, runtime_role: DB.dmRuntime, database_url_owner: `${POSTGRES}/${DB.dm}`, database_url_runtime: `postgres://${DB.dmRuntime}@127.0.0.1:${PORTS.postgres}/${DB.dm}`,
      log: join(PATHS.dmDir, 'logs', 'dev-api.log'), state_dir: join(PATHS.dmDir, 'dev'), bin_dir: PATHS.bin, build,
      speech: speechMode() === 'real'
        ? { mode: 'real', deepgram_url: 'https://api.deepgram.com', agent_url: 'wss://agent.deepgram.com/v1/agent/converse', key: `read from ${WAVEFORM_ENV} (WAVEFORM_DEEPGRAM_API_KEY) into dm-api's environment only` }
        : { mode: 'stand-in', deepgram_url: URLS.deepgram, agent_url: URLS.deepgramAgent, key: 'any value (the stand-in accepts every key)' },
      extra_env_file: PATHS.dmApiEnv,
      ting: tingOn ? { url: URLS.ting, app_id: 'ting' } : null,
      proof_issuers: 'dm.conversations.read, dm.messages.read, dm.messages.write, dm.receipts.write, dm.drafts.write = interface',
    },
    deepgram: {
      url: URLS.deepgram, listen_url: `${URLS.deepgram}/v1/listen`, agent_url: URLS.deepgramAgent, log: STAND_INS.deepgram.log,
      controls: { health: `${URLS.deepgram}/_health`, requests: `${URLS.deepgram}/_requests`, faults: `${URLS.deepgram}/_faults` },
    },
    briefcase: {
      url: URLS.briefcase, api_url: URLS.briefcase, api_base_url: `${URLS.briefcase}/api/v1`, public_url_form: `${URLS.briefcase}/api/v1/public/{owner}/{path}?view=inline`,
      data_dir: PATHS.briefcase, log: STAND_INS.briefcase.log, app_id: 'briefcase',
      controls: { health: `${URLS.briefcase}/_health`, state: `${URLS.briefcase}/_state`, requests: `${URLS.briefcase}/_requests`, faults: `${URLS.briefcase}/_faults` },
    },
    giphy: {
      url: URLS.giphy, api_base_url: `${URLS.giphy}/v1/gifs`, log: STAND_INS.giphy.log, empty_search_example: 'q=zzz',
      controls: { health: `${URLS.giphy}/_health`, requests: `${URLS.giphy}/_requests`, faults: `${URLS.giphy}/_faults` },
    },
    website: {
      app_id: 'dm', dev_url: URLS.webDev, e2e_url: URLS.webE2e, env_file: PATHS.webEnv, session_secret: secretsKept.website_session_secret,
      cookie_name: 'dm_session', redirect_uris: DM_REDIRECTS, uploads: { briefcase_api_url: URLS.briefcase, max_bytes: 200 * 1024 * 1024 },
    },
    relay: { home: PATHS.relay, port: PORTS.relay },
    identities: {
      file: PATHS.identities, helper,
      names: identities.map((identity) => ({ name: identity.name, id: identity.id, kind: identity.kind, ...(identity.custodian ? { custodian: identity.custodian } : {}) })),
      examples: {
        dm_token: `${helper} token ada --raw`,
        cli_login: `${helper} login scout`,
        cli_as: `${helper} dm scout -- messages send c:dmc-ada --text 'hello'`,
        slt: `${helper} slt scout | dm login --slt-stdin   (after eval "$(${helper} env scout)")`,
        website_cookie: `${helper} cookie ada`,
        storage_state: `${helper} storage-state ada --out test-results/.auth/ada.json`,
        proof: `${helper} proof ada --scope briefcase.uploads.reserve,briefcase.uploads.commit`,
      },
    },
    cli: { binary: join(PATHS.bin, 'dm'), per_identity: perIdentity },
    commands: {
      up: join(HERE, 'up.sh'), restart_dm: `${join(HERE, 'up.sh')} --restart-dm`, down: join(HERE, 'down.sh'), status: join(HERE, 'status.sh'),
      smoke: `node ${join(HERE, 'smoke.mjs')}`, real_speech: `DM_DEEPGRAM_REAL=1 ${join(HERE, 'up.sh')}`,
    },
    logs: { dir: PATHS.logs, dm_api: join(PATHS.dmDir, 'logs', 'dev-api.log'), accounts: ACCOUNTS_LOGS },
  };
}

function readSecretFile(path) {
  try {
    return readFileSync(path, 'utf8').trim() || null;
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------------------------------------ DM

function gitInfo() {
  const head = run(GIT, ['-C', ROOT, 'rev-parse', '--short', 'HEAD']).stdout.trim() || null;
  const dirty = run(GIT, ['-C', ROOT, 'status', '--porcelain']).stdout.split('\n').filter(Boolean).length;
  return { head, changed_files: dirty };
}

const DM_BINARIES = ['dm-api', 'dm-migrate', 'dm-worker', 'dm'];

function buildDm({ build }) {
  const source = join(SHARED_TARGET, 'debug');
  if (build) {
    const free = freeGiB();
    if (free !== null && free < 5) throw new StackError(`Only ${free.toFixed(1)} GiB are free on this disk; the build stops below 5 GiB.`, 'Free space first (the Carbon allowed deleting Rust build output), then run up.sh --restart-dm again.');
    say('building DM (cargo build -p silicon-dm --bins -p silicon-dm-cli; log .dev/logs/build.log)');
    // The build hygiene of spec §2.8: the shared target (every dependency is already compiled there), no incremental
    // state, the Command Line Tools instead of Xcode (whose licence is not accepted on this Mac).
    const env = { ...process.env, DEVELOPER_DIR: '/Library/Developer/CommandLineTools', CARGO_TARGET_DIR: SHARED_TARGET, CARGO_INCREMENTAL: '0' };
    env.PATH = `${process.env.PATH ?? ''}:${join(process.env.HOME ?? '', '.cargo', 'bin')}`;
    const done = run('cargo', ['build', '-p', 'silicon-dm', '--bins', '-p', 'silicon-dm-cli'], { cwd: ROOT, env, timeout: 60 * 60_000 });
    if (done.error?.code === 'ENOENT') throw new StackError('cargo was not found on PATH (nor in ~/.cargo/bin).', 'Install the pinned toolchain (rustup), or run up.sh from a shell where cargo works.');
    writePrivate(join(PATHS.logs, 'build.log'), `${done.stdout}\n${done.stderr}`);
    if (done.code !== 0) {
      throw new StackError(`cargo build failed, so dm-api keeps running the previous build (${buildInfo()?.built_at ?? 'none yet'}): ${tail(done.stderr, 20)}`, `Full output: ${join(PATHS.logs, 'build.log')}. Fix the build (or wait for whoever is changing it), then run up.sh --restart-dm again.`);
    }
  }
  ensureDir(PATHS.bin);
  for (const name of DM_BINARIES) {
    const from = join(source, name);
    if (!existsSync(from)) throw new StackError(`${from} does not exist.`, 'Build it: scripts/chat-stack/up.sh --restart-dm (without --no-build).');
    const temporary = join(PATHS.bin, `.${name}.new`);
    // A clone (APFS copy-on-write): a later cargo build never changes the binaries the stack runs.
    const copied = run('cp', ['-c', '-f', from, temporary]);
    if (copied.code !== 0) {
      const plain = run('cp', ['-f', from, temporary]);
      if (plain.code !== 0) throw new StackError(`Copying ${from} into ${PATHS.bin} failed: ${plain.stderr.trim()}`);
    }
    renameSync(temporary, join(PATHS.bin, name));
  }
  const info = { built_at: new Date().toISOString(), source, binaries: DM_BINARIES, binary_mtime: new Date(statSync(join(source, 'dm-api')).mtimeMs).toISOString(), git: gitInfo() };
  writeJsonPrivate(PATHS.build, info);
  return info;
}

function dmEnv(apps) {
  const mode = speechMode();
  const extra = extraDmEnv();
  const env = { PATH: process.env.PATH, HOME: process.env.HOME };
  if (mode === 'real') {
    if (!existsSync(WAVEFORM_ENV)) throw new StackError(`DM_DEEPGRAM_REAL=1 needs ${WAVEFORM_ENV}, which does not exist.`, 'Run without DM_DEEPGRAM_REAL to use the Deepgram stand-in.');
    const key = parseDotenv(readFileSync(WAVEFORM_ENV, 'utf8')).WAVEFORM_DEEPGRAM_API_KEY;
    if (!key) throw new StackError(`${WAVEFORM_ENV} has no WAVEFORM_DEEPGRAM_API_KEY.`, 'Run without DM_DEEPGRAM_REAL to use the Deepgram stand-in.');
    Object.assign(env, { DM_DEEPGRAM_API_KEY: key, DM_DEEPGRAM_URL: 'https://api.deepgram.com', DM_DEEPGRAM_AGENT_URL: 'wss://agent.deepgram.com/v1/agent/converse' });
  } else {
    Object.assign(env, { DM_DEEPGRAM_API_KEY: 'chat-stack-stand-in', DM_DEEPGRAM_URL: URLS.deepgram, DM_DEEPGRAM_AGENT_URL: URLS.deepgramAgent });
  }
  // GIF search and trending come from the Giphy stand-in (dev_accounts.py already gives dm-api a placeholder key).
  env.DM_GIPHY_API_BASE_URL = `${URLS.giphy}/v1/gifs`;
  Object.assign(env, extra);
  const names = ['DM_DEEPGRAM_API_KEY', 'DM_DEEPGRAM_URL', 'DM_DEEPGRAM_AGENT_URL', 'DM_GIPHY_API_BASE_URL', ...Object.keys(extra)];
  // What dm-api runs with, minus the key itself: a change here restarts it.
  // dm-api also reads DM's app secret (from the stack file): a new one at the Accounts stack restarts it too.
  const fingerprint = sha(JSON.stringify({ mode, extra, giphy: env.DM_GIPHY_API_BASE_URL, app: sha(apps.dm.secret) }));
  return { env, names, mode, fingerprint };
}

function devAccounts(subcommand, args, extraEnv = {}, passNames = []) {
  const env = {
    ...extraEnv,
    PATH: process.env.PATH, HOME: process.env.HOME,
    DM_TEST_STACK: PATHS.stack, DM_DEV_BASE: String(PORTS.web_dev), DM_DEV_POSTGRES_URL: POSTGRES, DM_DEV_DATABASE: DB.dm,
    DM_DEV_DIR: PATHS.dmDir, DM_DEV_BIN_DIR: PATHS.bin, DM_DEV_TING_APP: 'ting', DM_DEV_PASS_ENV: passNames.join(','),
    PSQL_BIN_DIR: PG_BIN,
  };
  const done = run('python3', ['-I', join(ROOT, 'scripts', 'dev_accounts.py'), subcommand, ...args], { cwd: ROOT, env, timeout: 10 * 60_000 });
  const lines = done.stdout.trim().split('\n');
  let summary = null;
  for (const line of lines.reverse()) {
    if (line.startsWith('{')) {
      try {
        summary = JSON.parse(line);
        break;
      } catch {
        // keep looking
      }
    }
  }
  for (const line of done.stdout.trim().split('\n')) if (line && !line.startsWith('{')) say(`dm: ${line}`);
  if (done.code !== 0) throw new StackError(`scripts/dev_accounts.py ${subcommand} failed: ${tail(done.stderr || done.stdout, 15)}`, `dm-api's log: ${join(PATHS.dmDir, 'logs', 'dev-api.log')}`);
  return summary;
}

async function ensureDm(state, { restart, apps }) {
  const speech = dmEnv(apps);
  const recorded = readJson(join(PATHS.dmDir, 'dev', 'chat-stack.json'), {});
  const ting = flag('ting');
  const running = livePidFrom(join(PATHS.dmDir, 'pids', 'dev-api'), 'dm-api');
  const changed = running && (recorded.fingerprint !== speech.fingerprint || Boolean(recorded.ting) !== ting);
  if (changed && !restart) say(`dm-api runs with other settings (speech ${recorded.mode ?? '?'}${recorded.ting ? ', Ting' : ''}): restarting it with speech ${speech.mode}${ting ? ', Ting' : ''}`);
  const fresh = flag('fresh-dm') || flag('reset-accounts');
  const args = [...(restart || changed || fresh ? ['--restart'] : []), ...(ting ? ['--ting'] : []), ...(fresh ? ['--fresh'] : [])];
  const summary = devAccounts('up', args, speech.env, speech.names);
  writeJsonPrivate(join(PATHS.dmDir, 'dev', 'chat-stack.json'), { mode: speech.mode, fingerprint: speech.fingerprint, ting, at: new Date().toISOString() });
  state.started.dm = true;
  return { summary, mode: speech.mode };
}

function livePidFrom(path, needle) {
  try {
    const pid = Number(readFileSync(path, 'utf8').trim());
    process.kill(pid, 0);
    const commandLine = run('ps', ['-o', 'command=', '-p', String(pid)]).stdout;
    return commandLine.includes(needle) ? pid : null;
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------------------------------------ stand-ins

function scriptFingerprint(path) {
  try {
    const files = [path, join(HERE, 'lib', 'common.mjs'), join(HERE, 'lib', 'ws.mjs')].filter(existsSync);
    return sha(files.map((file) => readFileSync(file, 'utf8')).join('\u0000'));
  } catch {
    return null;
  }
}

async function ensureStandIn(name, state, env) {
  const spec = STAND_INS[name];
  // Its code or its settings (e.g. briefcase's app secret) changed: restart it.
  const fingerprint = sha(`${scriptFingerprint(spec.script)}:${JSON.stringify(env)}`);
  const pid = livePid(name, spec.script);
  const healthy = (await http('GET', `http://127.0.0.1:${spec.port}/_health`, { timeoutMs: 2000 })).status === 200;
  if (pid && healthy && state.fingerprints?.[name] === fingerprint) return 'running';
  if (pid) {
    say(`restarting the ${name} stand-in (${healthy ? 'its code or settings changed' : 'it stopped answering'})`);
    await stopRecorded(name, spec.script);
  } else if (await portOpen(spec.port)) {
    const who = listener(spec.port);
    throw new StackError(`Port ${spec.port} (the ${name} stand-in) is taken by ${who ? `${who.command} (pid ${who.pid})` : 'another process'}.`, 'Stop that process; the chat stack owns this port.');
  }
  await startDetached(name, process.execPath, [spec.script, '--port', String(spec.port)], { env: { PATH: process.env.PATH, HOME: process.env.HOME, ...env }, log: spec.log });
  for (let i = 0; i < 50; i++) {
    if ((await http('GET', `http://127.0.0.1:${spec.port}/_health`, { timeoutMs: 1000 })).status === 200) {
      state.started[name] = true;
      state.fingerprints = { ...(state.fingerprints ?? {}), [name]: fingerprint };
      return 'started';
    }
    await sleep(200);
  }
  throw new StackError(`The ${name} stand-in did not answer on ${spec.port} within 10 s.`, `Its log: ${spec.log}`);
}

// ------------------------------------------------------------------------------------------------ the website

const MANAGED_WEB_KEYS = ['APP_ID', 'APP_SECRET', 'ACCOUNTS_URL', 'ACCOUNTS_API_URL', 'APP_API_URL', 'SESSION_SECRET', 'PUBLIC_URL', 'EXTRA_IMG_ORIGINS', 'BRIEFCASE_API_URL', 'UPLOADS_MAX_BYTES', 'MOCK_MESSAGING_URL'];

function writeWebEnv(apps, secretsKept) {
  const values = {
    APP_ID: 'dm',
    APP_SECRET: apps.dm.secret,
    ACCOUNTS_URL: URLS.accountsSite,
    ACCOUNTS_API_URL: URLS.accountsApi,
    APP_API_URL: URLS.dmApi,
    SESSION_SECRET: secretsKept.website_session_secret,
    PUBLIC_URL: URLS.webDev,
    EXTRA_IMG_ORIGINS: URLS.mockIris,
    BRIEFCASE_API_URL: URLS.briefcase,
    UPLOADS_MAX_BYTES: String(200 * 1024 * 1024),
    MOCK_MESSAGING_URL: URLS.mockMessaging,
  };
  const header = [
    '# Written by scripts/chat-stack/up.sh for the local chat stack (never commit it; .env* is git-ignored).',
    `# The dev website (pnpm dev, ${URLS.webDev}) and the Playwright production build (${URLS.webE2e}, which sets its own`,
    '# PUBLIC_URL) share these values, so one sealed session cookie works on both. Lines up.sh does not manage stay as they are.',
  ];
  const kept = [];
  if (existsSync(PATHS.webEnv)) {
    for (const line of readFileSync(PATHS.webEnv, 'utf8').split('\n')) {
      if (header.includes(line)) continue;
      const match = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=/.exec(line);
      if (match && MANAGED_WEB_KEYS.includes(match[1])) continue;
      kept.push(line);
    }
  }
  while (kept.length && !kept[kept.length - 1].trim()) kept.pop();
  const body = [...header, ...MANAGED_WEB_KEYS.map((key) => `${key}=${values[key]}`), ...(kept.length ? ['', ...kept] : [])].join('\n');
  writePrivate(PATHS.webEnv, `${body}\n`);
}

async function ensureWeb(state) {
  if (await portOpen(PORTS.web_dev)) return state.started.web && livePid('web', 'next') ? 'running (started by the stack)' : 'running (not started by the stack)';
  const webDir = join(ROOT, 'web');
  if (!existsSync(join(webDir, 'node_modules', '.bin', 'next'))) throw new StackError(`${webDir}/node_modules has no next.`, 'Install the website: pnpm -C web install --frozen-lockfile');
  await startDetached('web', join(webDir, 'node_modules', '.bin', 'next'), ['dev', '--port', String(PORTS.web_dev)], {
    cwd: webDir, env: { PATH: process.env.PATH, HOME: process.env.HOME, NEXT_TELEMETRY_DISABLED: '1', PORT: String(PORTS.web_dev) }, log: join(PATHS.logs, 'web.log'),
  });
  for (let i = 0; i < 180; i++) {
    const answer = await http('GET', `${URLS.webDev}/robots.txt`, { timeoutMs: 5000 });
    if (answer.status > 0) {
      state.started.web = true;
      return 'started';
    }
    await sleep(1000);
  }
  throw new StackError(`The dev website did not answer on ${PORTS.web_dev} within 3 minutes.`, `Its log: ${join(PATHS.logs, 'web.log')}`);
}

// ------------------------------------------------------------------------------------------------ the relay

function relayStatus() {
  const pool = readJson(PATHS.identities, null);
  const first = pool?.identities?.[0];
  if (!first || !existsSync(join(PATHS.bin, 'dm'))) return { running: false };
  const env = { PATH: `${PATHS.bin}:/usr/bin:/bin`, HOME: first.home, SILICON_HOME: first.home, SILICON_DM_RELAY_HOME: PATHS.relay, DM_API_URL: URLS.dmApi, ACCOUNTS_URL: URLS.accountsSite };
  const done = run(join(PATHS.bin, 'dm'), ['--json', 'daemon', 'status'], { env, timeout: 20_000 });
  try {
    // `dm daemon status --json`: {"running":bool,"pid":…,"host":{"port":…,"homes":…,"directory":…},…}
    const parsed = JSON.parse(done.stdout.trim().split('\n').pop());
    return { running: parsed.running === true, port: parsed.host?.port ?? null, pid: parsed.pid ?? null, version: parsed.version ?? null };
  } catch {
    return { running: false, error: tail(done.stderr || done.stdout, 3) };
  }
}

function startRelay() {
  const pool = readJson(PATHS.identities, null);
  const first = pool?.identities?.[0];
  if (!first) return { running: false };
  ensureDir(first.home);
  ensureDir(PATHS.relay);
  const env = { PATH: `${PATHS.bin}:/usr/bin:/bin`, HOME: first.home, SILICON_HOME: first.home, SILICON_DM_RELAY_HOME: PATHS.relay, DM_API_URL: URLS.dmApi, ACCOUNTS_URL: URLS.accountsSite };
  const done = run(join(PATHS.bin, 'dm'), ['--json', 'daemon', 'start', '--port', String(PORTS.relay)], { env, timeout: 60_000 });
  if (done.code !== 0) say(`the stack's dm relay did not start: ${tail(done.stderr || done.stdout, 4)}`);
  return relayStatus();
}

function stopRelay() {
  const pool = readJson(PATHS.identities, null);
  const first = pool?.identities?.[0];
  if (!first || !existsSync(join(PATHS.bin, 'dm'))) return false;
  const env = { PATH: `${PATHS.bin}:/usr/bin:/bin`, HOME: first.home, SILICON_HOME: first.home, SILICON_DM_RELAY_HOME: PATHS.relay, DM_API_URL: URLS.dmApi, ACCOUNTS_URL: URLS.accountsSite };
  return run(join(PATHS.bin, 'dm'), ['--json', 'daemon', 'stop'], { env, timeout: 60_000 }).code === 0;
}

// ------------------------------------------------------------------------------------------------ commands

async function up() {
  ensureDir(PATHS.dev);
  run('chmod', ['700', PATHS.dev]);
  for (const dir of [PATHS.logs, PATHS.pids, PATHS.homes, PATHS.relay, PATHS.briefcase, PATHS.accountsDir]) ensureDir(dir);
  const free = freeGiB();
  if (free !== null && free < 5) say(`warning: only ${free.toFixed(1)} GiB free on this disk`);
  const state = loadState();
  state.started ??= {};
  const report = {};

  report.postgres = ensurePostgres(state);
  saveState(state);
  if (flag('reset-accounts')) {
    // New Accounts data means new account uuids: DM's data, the pool, the CLI homes and the relay's queues all
    // describe the old accounts, so they go too and are made again below.
    say('--reset-accounts: dropping the Accounts and DM databases; the identity pool, CLI homes and relay queues are made again');
    stopRelay();
    for (const path of [PATHS.identities, PATHS.homes, PATHS.relay, PATHS.cache]) rmSync(path, { recursive: true, force: true });
    for (const dir of [PATHS.homes, PATHS.relay]) ensureDir(dir);
  }
  report.accounts = await ensureAccounts(state);
  saveState(state);
  const apps = fakeApps();
  const { ting } = await configureAccounts(apps);
  const secretsKept = secrets();
  // A first stack file now: scripts/dev_accounts.py reads the Accounts URLs and app secrets from it.
  writeJsonPrivate(PATHS.stack, composeStack({ apps, ting, secretsKept, state, pool: readJson(PATHS.identities, null), tingOn: flag('ting') }));
  writeWebEnv(apps, secretsKept);

  const restart = flag('restart-dm');
  const missing = DM_BINARIES.some((name) => !existsSync(join(PATHS.bin, name)));
  if (restart || missing) report.build = buildDm({ build: !flag('no-build') || missing && !existsSync(join(SHARED_TARGET, 'debug', 'dm-api')) });
  report.dm = await ensureDm(state, { restart, apps });
  saveState(state);

  report.deepgram = await ensureStandIn('deepgram', state, { DEEPGRAM_STANDIN_LOG: STAND_INS.deepgram.log });
  report.briefcase = await ensureStandIn('briefcase', state, {
    ACCOUNTS_API_URL: URLS.accountsApi, BRIEFCASE_APP_SECRET: apps.briefcase.secret, BRIEFCASE_STANDIN_DIR: PATHS.briefcase, BRIEFCASE_STANDIN_LOG: STAND_INS.briefcase.log,
    BRIEFCASE_PUBLIC_SITE_URL: URLS.briefcase,
  });
  report.giphy = await ensureStandIn('giphy', state, { GIPHY_STANDIN_LOG: STAND_INS.giphy.log });
  saveState(state);

  let pool = readJson(PATHS.identities, null);
  if (!flag('skip-identities')) {
    const done = run(process.execPath, [join(HERE, 'identities.mjs'), 'init'], { cwd: ROOT, env: process.env, timeout: 10 * 60_000 });
    for (const line of done.stderr.split('\n').filter(Boolean)) process.stderr.write(`${line}\n`);
    if (done.code !== 0) throw new StackError(`The identity pool could not be made: ${tail(done.stderr, 6)}`);
    pool = readJson(PATHS.identities, null);
    report.identities = `${pool.identities.length} identities`;
  }
  // A rebuilt dm must also be the relay: same-version relays are not replaced by the CLI itself.
  if (report.build) stopRelay();
  const relay = relayStatus();
  report.relay = relay.running ? `running on ${relay.port}` : (startRelay().running ? `started on ${PORTS.relay}` : 'not running (the first dm command starts it)');
  if (flag('web')) report.web = await ensureWeb(state);
  saveState(state);
  writeJsonPrivate(PATHS.stack, composeStack({ apps, ting, secretsKept, state, pool, tingOn: flag('ting') }));

  const build = buildInfo();
  const lines = [
    '',
    'DM chat stack is up',
    `  website (dev)        ${URLS.webDev}   ${report.web ? `(${report.web})` : '(not started here: pnpm -C web dev, or up.sh --web)'}`,
    `  website (e2e build)  ${URLS.webE2e}   (Playwright's production build; same web/.env.local)`,
    `  dm-api               ${URLS.dmApi}   (/live, /ready; speech ${report.dm.mode}; build ${build?.built_at ?? '?'} from ${build?.git?.head ?? '?'}${build?.git?.changed_files ? ` + ${build.git.changed_files} changed files` : ''})`,
    `  Silicon Accounts     ${URLS.accountsSite}   (API ${URLS.accountsApi}; mock email ${URLS.mockMessaging}/_messages)`,
    `  Deepgram stand-in    ${URLS.deepgram}   (/v1/listen, ${URLS.deepgramAgent})`,
    `  Briefcase stand-in   ${URLS.briefcase}   (/api/v1/obo/…, /api/v1/public/{owner}/{path})`,
    `  Giphy stand-in       ${URLS.giphy}   (dm-api's GIF trending and search)`,
    `  Postgres             ${POSTGRES}   (${DB.accounts}, ${DB.dm})`,
    `  identities           ${pool ? `${pool.identities.map((x) => x.name).join(', ')}` : 'none (--skip-identities)'}   (node scripts/chat-stack/identities.mjs list)`,
    `  dm relay             ${report.relay}`,
    `  everything else      ${PATHS.stack}   (0600: URLs, secrets, commands)`,
    '',
  ];
  process.stdout.write(lines.join('\n'));
}

async function down() {
  const state = loadState();
  state.started ??= {};
  const stopped = [];
  if (state.started.web && await stopRecorded('web', 'next')) stopped.push('website');
  state.started.web = false;
  if (stopRelay()) stopped.push('relay');
  if (existsSync(PATHS.stack) && livePidFrom(join(PATHS.dmDir, 'pids', 'dev-api'), 'dm-api')) {
    try {
      devAccounts('down', []);
      stopped.push('dm-api');
    } catch (error) {
      say(`dm-api: ${error.message}`);
    }
  }
  state.started.dm = false;
  for (const name of Object.keys(STAND_INS)) {
    if (await stopRecorded(name, STAND_INS[name].script)) stopped.push(`${name} stand-in`);
    state.started[name] = false;
  }
  if (!flag('keep-accounts') && existsSync(ACCOUNTS_RUN)) {
    const done = run('bash', [join(ACCOUNTS_CHECKOUT, 'scripts', 'stop.sh'), '--quiet'], { cwd: ACCOUNTS_CHECKOUT, env: { PATH: process.env.PATH, HOME: process.env.HOME, ACCOUNTS_PORT: String(PORTS.accounts_site) } });
    if (done.code === 0) stopped.push('Silicon Accounts');
    else say(`stopping Silicon Accounts said: ${tail(done.stderr || done.stdout, 4)}`);
    state.started.accounts = false;
  }
  if (flag('db')) {
    const done = run(join(PG_BIN, 'pg_ctl'), ['-D', PATHS.pg, '-m', 'fast', '-w', 'stop']);
    if (done.code === 0) stopped.push('Postgres');
    state.started.postgres = false;
  }
  saveState(state);
  process.stdout.write(`${JSON.stringify({ stopped, kept: [...(flag('keep-accounts') ? ['Silicon Accounts'] : []), ...(flag('db') ? [] : ['Postgres (5500)'])] })}\n`);
}

async function status() {
  const state = loadState();
  const parts = {};
  const pg = pgReady();
  parts.postgres = { ok: pg, url: POSTGRES, databases: pg ? Object.fromEntries([DB.accounts, DB.dm].map((name) => [name, psql('postgres', `SELECT 1 FROM pg_database WHERE datname = '${name}'`) === '1'])) : null };
  const accounts = await accountsHealth();
  parts.accounts = { ok: accounts.ok, public_url: URLS.accountsSite, api_url: URLS.accountsApi, checks: accounts.parts, site_mode: state.accounts_web ?? 'next' };
  const live = await http('GET', `${URLS.dmApi}/live`, { timeoutMs: 3000 });
  const ready = await http('GET', `${URLS.dmApi}/ready`, { timeoutMs: 5000 });
  const recorded = readJson(join(PATHS.dmDir, 'dev', 'chat-stack.json'), {});
  parts.dm_api = { ok: live.status === 204 && ready.status === 204, url: URLS.dmApi, live: live.status, ready: ready.status, pid: livePidFrom(join(PATHS.dmDir, 'pids', 'dev-api'), 'dm-api'), speech: recorded.mode ?? null, ting: Boolean(recorded.ting), build: buildInfo() };
  for (const name of Object.keys(STAND_INS)) {
    const answer = await http('GET', `http://127.0.0.1:${STAND_INS[name].port}/_health`, { timeoutMs: 2000 });
    parts[name] = { ok: answer.status === 200, url: `http://127.0.0.1:${STAND_INS[name].port}`, pid: livePid(name, STAND_INS[name].script), log: STAND_INS[name].log };
  }
  const pool = readJson(PATHS.identities, null);
  parts.identities = { ok: Boolean(pool?.identities?.length === 10 && pool.identities.every((x) => x.uuid)), file: PATHS.identities, count: pool?.identities?.length ?? 0 };
  const relay = relayStatus();
  parts.relay = { ok: relay.running, required: false, port: relay.port ?? null, home: PATHS.relay };
  for (const [name, url] of [['website_dev', URLS.webDev], ['website_e2e', URLS.webE2e]]) {
    const answer = await http('GET', `${url}/robots.txt`, { timeoutMs: 3000 });
    parts[name] = { ok: answer.status === 200, required: false, url, status: answer.status, started_by_stack: name === 'website_dev' ? Boolean(state.started?.web && livePid('web', 'next')) : false };
  }
  const required = ['postgres', 'accounts', 'dm_api', 'deepgram', 'briefcase', 'giphy', 'identities'];
  const ok = required.every((name) => parts[name].ok);
  process.stdout.write(`${JSON.stringify({ ok, required, stack_file: PATHS.stack, parts }, null, 2)}\n`);
  process.exitCode = ok ? 0 : 1;
}

async function main() {
  if (command === 'up') return withLock('stack', up, { onWait: (pid) => say(`waiting for another chat-stack command (pid ${pid}) to finish`) });
  if (command === 'down') return withLock('stack', down, { onWait: (pid) => say(`waiting for another chat-stack command (pid ${pid}) to finish`) });
  if (command === 'status') return status();
  const header = readFileSync(new URL(import.meta.url), 'utf8').split('\n').slice(1).filter((line) => line.startsWith('//')).map((line) => line.replace(/^\/\/ ?/, ''));
  process.stdout.write(`${header.join('\n')}\n`);
  if (command && !['help', '--help', '-h'].includes(command)) process.exitCode = 2;
  return undefined;
}

main().catch((error) => {
  process.stderr.write(`error: ${error.message}\n${error.hint ? `hint: ${error.hint}\n` : ''}`);
  if (!(error instanceof StackError) && error.stack) process.stderr.write(`${error.stack.split('\n').slice(1, 5).join('\n')}\n`);
  process.exitCode = 1;
});
