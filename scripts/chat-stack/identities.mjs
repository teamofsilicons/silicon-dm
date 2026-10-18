#!/usr/bin/env node
// The chat stack's identity pool: four Carbons and six Silicons at the local Silicon Accounts stack, with every
// credential a test needs made on demand, without spending email codes.
//
//   node scripts/chat-stack/identities.mjs <command> [who] [options]
//
//   init [--force]                 create the pool once (or repair it): Carbons and Silicons, admitted to DM
//   list [--json]                  names, ids, kinds, custodians and homes (no secrets)
//   show <who>                     one identity (no secrets)
//   token <who> [--raw] [--app dm] a NEW sign-in at DM: {access_token, refresh_token, ...}; --raw prints the token only
//   slt <who> [--app dm]           a fresh short-lived token (single use, 2 minutes): pipe it to dm login --slt-stdin
//   cookie <who> [--header] [--secure]
//                                  a sealed website session cookie (works on 4520 and 4530: same SESSION_SECRET)
//   storage-state <who> [--out F]  a Playwright storageState: the website cookie (+ a Carbon's Accounts session)
//   env <who>                      shell exports that point the dm CLI at the stack as <who>, in its own home
//   login <who>                    signs the dm CLI in as <who> in its home (Silicon: SLT; Carbon: device flow)
//   dm <who> [--] <dm arguments>   runs the dm CLI as <who> (DM_BIN=<path> runs another build)
//   proof <who> --scope S[,S] [--to briefcase] [--ttl 600]
//                                  a User verification proof that dm acts for <who> at the receiving app
//   first-party <silicon>          a first-party Silicon Accounts token for a Silicon (aud silicon-accounts)
//   init-demo [--fresh | --set N]  the demo workspace's accounts (once, or repaired): Mira Patel (c:mira) and the
//                                  Silicons she looks after, Sol and Wren, signed in to the CLI; --fresh: a new set
//                                  (c:mira-2, …); --set N: back to set N. scripts/chat-stack/seed-demo.mjs runs it.
//
// <who> is a name (ada, bo, cy, di, scout, quill, atlas, pip, ember, nova; the demo's mira, sol, wren), an id (c:dmc-ada,
// si:dmc-scout, or the short c:ada / si:scout) or a uuid. Every token, cookie and proof is printed only by the command
// that asks for it.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, chmodSync } from 'node:fs';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { ACCOUNTS_CHECKOUT, HERE, PATHS, StackError, describe, http, readJson, stackFile, withLock, writePrivate } from './lib/common.mjs';
import {
  CARBONS, SILICONS, appTokens, carbonId, cliEnv, dmBinary, dmCall, emailOf, findIdentity, homeOf,
  issueProof, loadPool, mintSlt, savePool, sessionHeaders, siliconFirstParty, siliconId, stackUrls, websiteCookie,
  DEMO_CARBONS, DEMO_SILICONS, demoHandle, withDemo,
} from './lib/pool.mjs';

const argv = process.argv.slice(2);
const command = argv[0];

function option(name, fallback) {
  const index = argv.indexOf(`--${name}`);
  if (index < 0) return fallback;
  const value = argv[index + 1];
  if (value === undefined || value.startsWith('--')) throw new StackError(`--${name} needs a value.`);
  return value;
}
const flag = (name) => argv.includes(`--${name}`);
const positional = () => {
  const values = [];
  for (let i = 1; i < argv.length; i++) {
    if (argv[i] === '--') break;
    if (argv[i].startsWith('--')) {
      if (!['raw', 'json', 'force', 'header', 'secure'].includes(argv[i].slice(2))) i++;
      continue;
    }
    values.push(argv[i]);
  }
  return values;
};
const print = (value) => process.stdout.write(`${typeof value === 'string' ? value : JSON.stringify(value)}\n`);
const say = (text) => process.stderr.write(`identities: ${text}\n`);
const shellQuote = (value) => `'${String(value).replace(/'/g, `'\\''`)}'`;

// ------------------------------------------------------------------------------------------------ init

async function carbonSessionWorks(identity, stack) {
  if (!identity?.accounts_session?.cookies?.length) return false;
  const { api, site } = stackUrls(stack);
  const answer = await http('GET', `${api}/v1/me`, { headers: sessionHeaders(identity, site) });
  return answer.status === 200 && answer.json?.id === identity.id;
}

/** Signs a Carbon in on the account site with one email code (creating it when new); returns its pool entry. */
function signInCarbon(spec, stack) {
  const tsx = join(ACCOUNTS_CHECKOUT, 'testkit', 'node_modules', '.bin', 'tsx');
  if (!existsSync(tsx)) throw new StackError(`${tsx} does not exist.`, `Install the testkit: pnpm -C ${ACCOUNTS_CHECKOUT}/testkit install --frozen-lockfile`);
  const { api } = stackUrls(stack);
  const done = spawnSync(tsx, [join(HERE, 'lib', 'carbon-signin.mts'), '--accounts-dir', ACCOUNTS_CHECKOUT, '--api', api,
    '--messaging', stack.accounts.mock_messaging_url, '--email', emailOf(spec), '--id', carbonId(spec), '--name', spec.display_name], {
    encoding: 'utf8', timeout: 120_000, env: { PATH: process.env.PATH, HOME: process.env.HOME },
  });
  if (done.status !== 0) {
    throw new StackError(`Signing ${carbonId(spec)} in on the account site failed: ${(done.stderr || done.stdout || '').trim().split('\n').slice(-6).join(' | ')}`,
      'Check that the Accounts stack answers (scripts/chat-stack/status.sh) and that its email-code limit was not hit (30 per network in 10 minutes).');
  }
  const result = JSON.parse(done.stdout.trim().split('\n').pop());
  if (result.id !== carbonId(spec)) throw new StackError(`The account site signed ${emailOf(spec)} in as ${result.id}, not ${carbonId(spec)}.`, 'Another account holds that email or id at this Accounts database; reset it with scripts/chat-stack/up.sh --reset-accounts.');
  return { uuid: result.uuid, display_name: result.display_name, accounts_session: { cookies: result.cookies } };
}

async function ensureSilicon(spec, custodian, existing, stack) {
  const { api, site } = stackUrls(stack);
  const id = siliconId(spec);
  if (existing?.stk) {
    const answer = await http('POST', `${api}/v1/silicons/login`, { json: { id, stk: existing.stk, client_label: 'chat stack pool check' } });
    if (answer.status === 200) return { uuid: existing.uuid, stk: existing.stk };
  }
  const headers = sessionHeaders(custodian, site);
  const listed = await http('GET', `${api}/v1/me/silicons?limit=100`, { headers });
  if (listed.status !== 200) throw new StackError(`Listing ${custodian.id}'s Silicons failed: ${describe(listed)}`);
  const found = (listed.json.items ?? []).find((item) => item.id === id);
  if (found) {
    const rotated = await http('POST', `${api}/v1/me/silicons/${encodeURIComponent(found.uuid)}/stk`, { headers, json: {} });
    if (rotated.status !== 200 || !rotated.json?.stk) throw new StackError(`Rotating the STK of ${id} failed: ${describe(rotated)}`);
    say(`${id} existed without a usable STK in the pool: rotated it`);
    return { uuid: found.uuid, stk: rotated.json.stk };
  }
  const created = await http('POST', `${api}/v1/me/silicons`, {
    headers: { ...headers, 'idempotency-key': `chat-stack-${randomUUID()}` },
    json: { id, display_name: spec.display_name, timezone: 'UTC' },
  });
  if (created.status !== 201) throw new StackError(`Creating ${id} for ${custodian.id} failed: ${describe(created)}`);
  say(`created ${id} (custodian ${custodian.id})`);
  return { uuid: created.json.silicon.uuid, stk: created.json.stk };
}

async function init() {
  const stack = stackFile();
  const force = flag('force');
  const previous = readJson(PATHS.identities, { version: 1, identities: [] });
  const byName = new Map((previous.identities ?? []).map((x) => [x.name, x]));
  const pool = { version: 1, created_at: previous.created_at ?? new Date().toISOString(), updated_at: new Date().toISOString(),
    accounts_public_url: stack.accounts.public_url, accounts_api_url: stack.accounts.api_url, dm_api_url: stack.dm.api_url, identities: [] };
  // The demo workspace's accounts (init-demo) are kept as they are: this command makes and checks only the ten above.
  if (previous.demo) pool.demo = previous.demo;

  for (const spec of CARBONS) {
    const id = carbonId(spec);
    const existing = byName.get(spec.name);
    let entry;
    if (!force && existing && await carbonSessionWorks(existing, stack)) {
      entry = existing;
    } else {
      say(`signing ${id} in on the account site (one email code)`);
      entry = { ...signInCarbon(spec, stack) };
    }
    pool.identities.push({
      name: spec.name, kind: 'carbon', id, uuid: entry.uuid, display_name: spec.display_name, email: emailOf(spec),
      home: homeOf(id), silicons: SILICONS.filter((s) => s.custodian === spec.name).map(siliconId),
      accounts_session: entry.accounts_session,
    });
    savePool({ ...previous, ...pool, identities: [...pool.identities, ...(previous.identities ?? []).filter((x) => !pool.identities.some((y) => y.name === x.name))] });
  }
  for (const spec of SILICONS) {
    const custodian = pool.identities.find((x) => x.name === spec.custodian);
    const made = await ensureSilicon(spec, custodian, byName.get(spec.name), stack);
    pool.identities.push({
      name: spec.name, kind: 'silicon', id: siliconId(spec), uuid: made.uuid, display_name: spec.display_name,
      custodian: custodian.id, home: homeOf(siliconId(spec)), stk: made.stk,
    });
    savePool(pool);
  }

  // DM learns every account on its first request. Nobody needs allowing: anyone may message any Silicon.
  for (const identity of pool.identities) {
    const signedIn = await appTokens(identity, 'dm', stack);
    const me = await dmCall(signedIn.access_token, 'GET', '/api/v1/me', undefined, { stack });
    if (me.status !== 200) throw new StackError(`DM did not admit ${identity.id}: ${describe(me)}`, 'Is dm-api up? scripts/chat-stack/status.sh');
  }
  for (const identity of pool.identities) {
    mkdirSync(identity.home, { recursive: true, mode: 0o700 });
    chmodSync(identity.home, 0o700);
  }
  savePool(pool);
  print({ pool: PATHS.identities, carbons: pool.identities.filter((x) => x.kind === 'carbon').map((x) => x.id), silicons: pool.identities.filter((x) => x.kind === 'silicon').map((x) => `${x.id} (custodian ${x.custodian})`) });
}

// ------------------------------------------------------------------------------------------------ init-demo

/** Whether the dm CLI in the identity's own home is signed in as it (`dm login status`). */
async function cliSignedIn(identity, stack) {
  if (!existsSync(dmBinary())) return false;
  const done = await runDm(identity, ['--json', 'login', 'status'], { stack });
  try {
    const status = JSON.parse(done.stdout.trim().split('\n').pop());
    return status.authenticated === true && status.id === identity.id;
  } catch {
    return false;
  }
}

/**
 * The demo workspace's accounts, made once and checked on every run, the way init makes the pool: the Carbon keeps
 * her account-site session (one email code, the first time), each Silicon its STK; DM admits them, and each Silicon's
 * CLI is signed in in its own home (it speaks with `dm messages send --speak`). They are written under `demo` in the
 * pool file, holding the stack's lock so a concurrent up.sh never loses them.
 */
async function initDemo() {
  const stack = stackFile();
  const fresh = flag('fresh');
  const requested = argv.includes('--set') ? Number(option('set')) : null;
  if (requested !== null && (!Number.isInteger(requested) || requested < 1)) throw new StackError(`--set ${option('set')} is not a set number.`, 'Sets are numbered from 1: --set 1 is c:mira, si:sol and si:wren.');
  return withLock('stack', async () => {
    const pool = loadPool();
    const previous = pool.demo ?? null;
    // Earlier sets stay at the stack (their accounts and conversations), kept whole here, so --set N goes back to one
    // without an email code; --fresh starts the next number.
    let retired = [...(previous?.retired ?? [])].filter((entry) => Array.isArray(entry.identities));
    let set = previous?.set ?? 1;
    let start = previous?.identities ?? [];
    let createdAt = previous?.created_at;
    if (fresh || (requested && requested !== set)) {
      if (previous) retired.push({ set: previous.set, created_at: previous.created_at, identities: previous.identities });
      set = fresh ? Math.max(set, ...retired.map((entry) => entry.set)) + 1 : requested;
      const returning = retired.find((entry) => entry.set === set);
      start = returning?.identities ?? [];
      createdAt = returning?.created_at;
      retired = retired.filter((entry) => entry.set !== set);
    }
    const byName = new Map(start.map((x) => [x.name, x]));
    const now = new Date().toISOString();
    const demo = { set, created_at: createdAt ?? now, updated_at: now, identities: [], retired };
    const save = () => savePool({ ...loadPool(), demo: { ...demo, identities: [...demo.identities, ...start.filter((x) => !demo.identities.some((y) => y.name === x.name))] } });
    const specOf = (base) => ({ ...base, handle: demoHandle(base, set) });

    for (const base of DEMO_CARBONS) {
      const spec = specOf(base);
      const id = carbonId(spec);
      const existing = byName.get(spec.name);
      let entry;
      if (existing?.id === id && await carbonSessionWorks(existing, stack)) {
        entry = existing;
      } else {
        say(`signing ${id} in on the account site (one email code)`);
        entry = signInCarbon(spec, stack);
      }
      demo.identities.push({
        name: spec.name, kind: 'carbon', id, uuid: entry.uuid, display_name: spec.display_name, email: emailOf(spec), home: homeOf(id),
        silicons: DEMO_SILICONS.filter((s) => s.custodian === spec.name).map((s) => siliconId(specOf(s))), accounts_session: entry.accounts_session, demo: true,
      });
      save();
    }
    for (const base of DEMO_SILICONS) {
      const spec = specOf(base);
      const custodian = demo.identities.find((x) => x.name === spec.custodian);
      const existing = byName.get(spec.name);
      const made = await ensureSilicon(spec, custodian, existing?.id === siliconId(spec) ? existing : null, stack);
      demo.identities.push({
        name: spec.name, kind: 'silicon', id: siliconId(spec), uuid: made.uuid, display_name: spec.display_name, custodian: custodian.id,
        home: homeOf(siliconId(spec)), stk: made.stk, demo: true,
      });
      save();
    }

    // DM learns each account on its first request.
    for (const identity of demo.identities) {
      const signedIn = await appTokens(identity, 'dm', stack);
      const me = await dmCall(signedIn.access_token, 'GET', '/api/v1/me', undefined, { stack });
      if (me.status !== 200) throw new StackError(`DM did not admit ${identity.id}: ${describe(me)}`, 'Is dm-api up? scripts/chat-stack/status.sh');
    }
    for (const identity of demo.identities) {
      mkdirSync(identity.home, { recursive: true, mode: 0o700 });
      chmodSync(identity.home, 0o700);
    }
    // The Silicons use the CLI like any Silicon: signed in once in their own homes (an SLT each, no email code).
    const signedInCli = [];
    for (const identity of demo.identities.filter((x) => x.kind === 'silicon')) {
      if (await cliSignedIn(identity, stack)) continue;
      const slt = await mintSlt(identity, 'dm', stack);
      const done = await runDm(identity, ['--json', 'login', '--slt-stdin', '--label', 'chat stack demo'], { stdin: slt, stack });
      if (done.code !== 0) throw new StackError(`dm login failed for ${identity.id} (exit ${done.code}): ${(done.stderr || done.stdout).trim().slice(-600)}`, 'Check the CLI snapshot: scripts/chat-stack/status.sh (dm_api.build).');
      signedInCli.push(identity.id);
    }
    save();
    print({
      pool: PATHS.identities, set,
      carbons: demo.identities.filter((x) => x.kind === 'carbon').map((x) => `${x.id} (${x.display_name})`),
      silicons: demo.identities.filter((x) => x.kind === 'silicon').map((x) => `${x.id} (${x.display_name}, custodian ${x.custodian})`),
      cli_signed_in_now: signedInCli,
      ...(demo.retired.length ? { retired_sets: demo.retired.map((x) => x.set) } : {}),
    });
  }, { onWait: (pid) => say(`waiting for another chat-stack command (pid ${pid}) to finish`) });
}

// ------------------------------------------------------------------------------------------------ commands

function publicView(identity) {
  const { accounts_session: _session, stk: _stk, ...rest } = identity;
  return rest;
}

async function runDm(identity, args, { stdin, inherit = false, stack = stackFile() } = {}) {
  const binary = dmBinary();
  if (!existsSync(binary)) throw new StackError(`${binary} does not exist.`, 'Build and snapshot it: scripts/chat-stack/up.sh --restart-dm (or set DM_BIN to a dm binary).');
  mkdirSync(homeOf(identity.id), { recursive: true, mode: 0o700 });
  mkdirSync(PATHS.relay, { recursive: true, mode: 0o700 });
  return new Promise((resolve, reject) => {
    const child = spawn(binary, args, { env: cliEnv(identity, stack), stdio: inherit ? 'inherit' : ['pipe', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    if (!inherit) {
      child.stdout.on('data', (chunk) => { stdout += chunk; });
      child.stderr.on('data', (chunk) => { stderr += chunk; });
      if (stdin !== undefined) child.stdin.end(stdin);
      else child.stdin.end();
    }
    child.on('error', reject);
    child.on('close', (code) => resolve({ code, stdout, stderr }));
  });
}

async function login(identity) {
  const stack = stackFile();
  const label = 'chat stack';
  if (identity.kind === 'silicon') {
    const slt = await mintSlt(identity, 'dm', stack);
    const done = await runDm(identity, ['--json', 'login', '--slt-stdin', '--label', label], { stdin: slt, stack });
    if (done.code !== 0) throw new StackError(`dm login failed for ${identity.id} (exit ${done.code}): ${(done.stderr || done.stdout).trim().slice(-600)}`);
    process.stdout.write(done.stdout);
    return;
  }
  // A Carbon uses the device flow: dm prints the code, the Carbon's browser session approves it.
  const binary = dmBinary();
  const { api, site } = stackUrls(stack);
  await new Promise((resolve, reject) => {
    mkdirSync(homeOf(identity.id), { recursive: true, mode: 0o700 });
    const child = spawn(binary, ['--json', 'login', '--label', label], { env: cliEnv(identity, stack), stdio: ['ignore', 'pipe', 'pipe'] });
    let buffered = '';
    let stderr = '';
    let approved = false;
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.stdout.on('data', async (chunk) => {
      buffered += chunk;
      if (approved) return;
      const line = buffered.split('\n').find((candidate) => candidate.includes('"device_code"'));
      if (!line) return;
      approved = true;
      const event = JSON.parse(line);
      const answer = await http('POST', `${api}/v1/device/${encodeURIComponent(event.user_code)}/approve`, { headers: sessionHeaders(identity, site), json: {} });
      if (answer.status !== 204 && answer.status !== 200) {
        child.kill();
        reject(new StackError(`Approving ${identity.id}'s device code failed: ${describe(answer)}`));
      }
    });
    child.on('error', reject);
    child.on('close', (code) => {
      if (code !== 0) return reject(new StackError(`dm login (device flow) failed for ${identity.id} (exit ${code}): ${(stderr || buffered).trim().slice(-600)}`));
      process.stdout.write(buffered.split('\n').filter((line) => line && !line.includes('"device_code"')).join('\n') + '\n');
      resolve();
    });
  });
}

async function main() {
  if (!command || command === 'help' || command === '--help' || command === '-h') {
    const { readFileSync } = await import('node:fs');
    const header = readFileSync(new URL(import.meta.url), 'utf8').split('\n').slice(1).filter((line) => line.startsWith('//')).map((line) => line.replace(/^\/\/ ?/, ''));
    process.stdout.write(`${header.join('\n')}\n`);
    return;
  }
  if (command === 'init') return init();
  if (command === 'init-demo') return initDemo();
  // The demo workspace's accounts (init-demo) are found by every command, after the pool's own ten.
  const pool = withDemo(loadPool());
  if (command === 'list') {
    const rows = pool.identities.map(publicView);
    if (flag('json')) return print(rows);
    for (const row of rows) {
      process.stdout.write(`${row.name.padEnd(6)} ${row.id.padEnd(14)} ${row.kind.padEnd(8)} ${(row.kind === 'silicon' ? `custodian ${row.custodian}` : `looks after ${row.silicons.join(', ') || 'no Silicons'}`).padEnd(44)} ${row.display_name}\n`);
    }
    return;
  }
  const [who] = positional();
  const identity = findIdentity(pool, who);
  const stack = stackFile();
  switch (command) {
    case 'show':
      return print(publicView(identity));
    case 'token': {
      const tokens = await appTokens(identity, option('app', 'dm'), stack);
      if (flag('raw')) return print(tokens.access_token);
      return print({
        access_token: tokens.access_token, refresh_token: tokens.refresh_token, scope: tokens.scope,
        expires_at: tokens.access_expires_at, refresh_expires_at: tokens.refresh_expires_at, account: tokens.account,
      });
    }
    case 'slt':
      return print(await mintSlt(identity, option('app', 'dm'), stack));
    case 'first-party':
      if (identity.kind !== 'silicon') throw new StackError('first-party is for Silicons (their STK signs them in).', 'For a Carbon, use token (a DM sign-in) or slt.');
      return print(await siliconFirstParty(identity, stack));
    case 'cookie': {
      const cookie = await websiteCookie(identity, { stack, secure: flag('secure') });
      if (flag('header')) return print(`${cookie.name}=${cookie.value}`);
      return print({ name: cookie.name, value: cookie.value, domain: '127.0.0.1', path: '/', httpOnly: true, secure: flag('secure'), sameSite: 'Lax', expires: cookie.expires, account: cookie.account });
    }
    case 'storage-state': {
      const cookie = await websiteCookie(identity, { stack });
      const cookies = ['127.0.0.1', 'localhost'].map((domain) => ({ name: cookie.name, value: cookie.value, domain, path: '/', expires: cookie.expires, httpOnly: true, secure: false, sameSite: 'Lax' }));
      if (identity.kind === 'carbon') {
        // The Carbon's Silicon Accounts browser session too, so the hosted pages offer "Continue as" without a code.
        for (const accountsCookie of identity.accounts_session?.cookies ?? []) {
          for (const domain of ['localhost', '127.0.0.1']) {
            cookies.push({ name: accountsCookie.name, value: accountsCookie.value, domain, path: '/', expires: Math.floor((accountsCookie.expires_ms ?? Date.now() + 86400_000) / 1000), httpOnly: true, secure: false, sameSite: 'Lax' });
          }
        }
      }
      const state = { cookies, origins: [] };
      const out = option('out');
      if (out) {
        writePrivate(out, `${JSON.stringify(state, null, 2)}\n`);
        return print({ storage_state: out, account: cookie.account });
      }
      return print(state);
    }
    case 'env': {
      const env = cliEnv(identity, stack);
      for (const name of ['SILICON_HOME', 'SILICON_DM_RELAY_HOME', 'DM_API_URL', 'ACCOUNTS_URL']) process.stdout.write(`export ${name}=${shellQuote(env[name])}\n`);
      process.stdout.write(`export PATH=${shellQuote(PATHS.bin)}:"$PATH"\n`);
      return undefined;
    }
    case 'login':
      return login(identity);
    case 'dm': {
      const separator = argv.indexOf('--');
      const args = separator >= 0 ? argv.slice(separator + 1) : argv.slice(2);
      const done = await runDm(identity, args, { inherit: true, stack });
      process.exitCode = done.code ?? 1;
      return undefined;
    }
    case 'proof': {
      const scopes = option('scope', '').split(',').map((s) => s.trim()).filter(Boolean);
      if (!scopes.length) throw new StackError('proof needs --scope (comma separated), e.g. --scope briefcase.uploads.reserve,briefcase.uploads.commit');
      return print(await issueProof(identity, { receiving: option('to', 'briefcase'), scopes, ttl: Number(option('ttl', '600')) }, stack));
    }
    default:
      throw new StackError(`Unknown command ${command}.`, 'node scripts/chat-stack/identities.mjs help');
  }
}

main().catch((error) => {
  process.stderr.write(`error: ${error.message}\n${error.hint ? `hint: ${error.hint}\n` : ''}`);
  if (!(error instanceof StackError) && error.stack) process.stderr.write(`${error.stack.split('\n').slice(1, 4).join('\n')}\n`);
  process.exitCode = 1;
});
