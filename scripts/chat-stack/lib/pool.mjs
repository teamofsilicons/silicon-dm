// The identity pool of the chat stack: four Carbons and six Silicons at the local Silicon Accounts stack, and the
// helpers that turn them into credentials without spending email codes.
//
// How a credential is made (nothing here talks to a deployed service; every URL must be on this machine):
//   Carbon   its account-site browser session (the sa_session cookie, 900 days, never rotates) -> POST
//            /v1/me/short-lived-tokens -> an SLT for an app -> the app exchanges it (POST /v1/oauth/token, app secret)
//   Silicon  its STK -> POST /v1/silicons/login (a first-party token) -> the same SLT and exchange
// Each `token`/`cookie` call is a new sign-in of its own (its own refresh token), so parallel tests never share a
// rotating refresh token and never trip reuse detection.
import { createCipheriv, createHash, hkdfSync, randomBytes, randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { PATHS, ROOT, StackError, describe, http, readJson, stackFile, writeJsonPrivate, LOOPBACK } from './common.mjs';

/** Who is in the pool. Names are short aliases; ids are the real `c:`/`si:` ids at the stack. */
export const CARBONS = [
  { name: 'ada', handle: 'dmc-ada', display_name: 'Ada Marlow' },
  { name: 'bo', handle: 'dmc-bo', display_name: 'Bo Okafor' },
  { name: 'cy', handle: 'dmc-cy', display_name: 'Cy Varga' },
  { name: 'di', handle: 'dmc-di', display_name: 'Di Tanaka' },
];
export const SILICONS = [
  { name: 'scout', handle: 'dmc-scout', display_name: 'Scout', custodian: 'ada' },
  { name: 'quill', handle: 'dmc-quill', display_name: 'Quill', custodian: 'ada' },
  { name: 'atlas', handle: 'dmc-atlas', display_name: 'Atlas', custodian: 'bo' },
  { name: 'pip', handle: 'dmc-pip', display_name: 'Pip', custodian: 'bo' },
  { name: 'ember', handle: 'dmc-ember', display_name: 'Ember', custodian: 'cy' },
  { name: 'nova', handle: 'dmc-nova', display_name: 'Nova', custodian: 'cy' },
];
// Anyone signed in to DM may message any Silicon (since DM 0.17.1), so the pool sets no allow-lists: Di, who looks
// after nobody, is a stranger to every Silicon and can still reach each of them.

/**
 * The demo workspace's own accounts (scripts/chat-stack/seed-demo.mjs): a Carbon no test signs in as, with the two
 * Silicons she looks after, so screenshots show DM as a Carbon sees it instead of test data. They live under `demo` in
 * the pool file, never in `identities` (status.sh counts the ten accounts there, and `init` rewrites that list), and
 * every identities.mjs command finds them by name too (withDemo). `init-demo --fresh` makes a new set of them: set 1
 * keeps the plain handles (c:mira, si:sol), set 2 is c:mira-2 and so on, with the same display names.
 */
export const DEMO_CARBONS = [{ name: 'mira', handle: 'mira', display_name: 'Mira Patel' }];
export const DEMO_SILICONS = [
  { name: 'sol', handle: 'sol', display_name: 'Sol', custodian: 'mira' },
  { name: 'wren', handle: 'wren', display_name: 'Wren', custodian: 'mira' },
];
/** A demo account's handle in set `set`. */
export const demoHandle = (spec, set) => (set > 1 ? `${spec.handle}-${set}` : spec.handle);
/** The pool with the demo accounts findable as well (findIdentity looks in `identities`). */
export const withDemo = (pool) => ({ ...pool, identities: [...(pool.identities ?? []), ...(pool.demo?.identities ?? [])] });

export const carbonId = (spec) => `c:${spec.handle}`;
export const siliconId = (spec) => `si:${spec.handle}`;
export const emailOf = (spec) => `${spec.handle}@example.test`;
/** Each identity's own SILICON_HOME for the dm CLI (the directory is named by its id, as `dm` names accounts). */
export const homeOf = (id) => join(PATHS.homes, id);

// ------------------------------------------------------------------------------------------------ the file

export function loadPool() {
  const pool = readJson(PATHS.identities, null);
  if (!pool || !Array.isArray(pool.identities)) {
    throw new StackError(`${PATHS.identities} does not exist yet (or is not a pool file).`, 'Create the pool: node scripts/chat-stack/identities.mjs init (scripts/chat-stack/up.sh does it for you).');
  }
  return pool;
}

export function savePool(pool) {
  writeJsonPrivate(PATHS.identities, pool);
}

/** Finds an identity by name (ada), id (c:dmc-ada, also without the dmc- part: c:ada) or uuid. */
export function findIdentity(pool, who) {
  if (!who) throw new StackError('Name an identity.', `One of: ${pool.identities.map((x) => x.name).join(', ')} (or its id or uuid).`);
  const wanted = String(who).trim().toLowerCase();
  const found = pool.identities.find((x) => x.name === wanted || x.id === wanted || x.uuid === wanted
    || x.id === wanted.replace(/^(c|si):(?!dmc-)/, '$1:dmc-'));
  if (!found) throw new StackError(`No identity named ${who} in the pool.`, `Use one of: ${pool.identities.map((x) => `${x.name} (${x.id})`).join(', ')}.`);
  return found;
}

// ------------------------------------------------------------------------------------------------ Accounts calls

export function stackUrls(stack = stackFile()) {
  const accounts = stack.accounts ?? {};
  const api = accounts.api_url ?? stack.accounts_api_url;
  const site = accounts.public_url ?? stack.accounts_public_url;
  for (const url of [api, site]) {
    if (!url || !LOOPBACK.has(new URL(url).hostname)) throw new StackError(`The stack file names ${url} for Silicon Accounts, which is not on this machine.`, 'The pool only ever talks to the local stack; rerun scripts/chat-stack/up.sh.');
  }
  return { api, site, dm: stack.dm?.api_url ?? 'http://127.0.0.1:4521' };
}

export function appSecret(stack, app) {
  const secret = stack.apps?.[app]?.app_secret;
  if (!secret) throw new StackError(`The stack file has no secret for the app ${app}.`, `Known apps: ${Object.keys(stack.apps ?? {}).join(', ')}.`);
  return secret;
}

/** Headers that make a call as the Carbon's account-site browser session (the cookie and the Origin guard). */
export function sessionHeaders(identity, site) {
  const cookies = identity.accounts_session?.cookies ?? [];
  if (!cookies.length) throw new StackError(`${identity.id} has no account-site session in the pool.`, 'Repair the pool: node scripts/chat-stack/identities.mjs init');
  return { cookie: cookies.map((c) => `${c.name}=${encodeURIComponent(c.value)}`).join('; '), origin: site };
}

const FIRST_PARTY_CACHE = join(PATHS.cache, 'first-party.json');

/**
 * A first-party access token (aud silicon-accounts) for a Silicon, from its STK. Kept in .dev/cache (0600) until two
 * minutes before it expires: Silicon Accounts allows 60 STK sign-ins a minute per network, and a test run asking for
 * many tokens would otherwise spend them. Each sign-in also carries its own made-up client address
 * (X-Forwarded-For, which the local stack trusts), so parallel runs never share that budget.
 */
export async function siliconFirstParty(identity, stack = stackFile()) {
  const { api } = stackUrls(stack);
  const stkTag = createHash('sha256').update(identity.stk ?? '').digest('hex').slice(0, 16);
  const cache = readJson(FIRST_PARTY_CACHE, {});
  const kept = cache[identity.uuid];
  if (kept && kept.stk === stkTag && kept.api === api && Date.parse(kept.expires_at) - Date.now() > 120_000) return kept.access_token;
  const address = `10.${randomBytes(3).join('.')}`;
  const answer = await http('POST', `${api}/v1/silicons/login`, {
    headers: { 'x-forwarded-for': address }, json: { id: identity.id, stk: identity.stk, client_label: 'chat stack pool' },
  });
  if (answer.status !== 200 || !answer.json?.access_token) throw new StackError(`Signing ${identity.id} in with its STK failed: ${describe(answer)}`, 'Repair the pool: node scripts/chat-stack/identities.mjs init (it rotates a lost STK).');
  const fresh = readJson(FIRST_PARTY_CACHE, {});
  fresh[identity.uuid] = { access_token: answer.json.access_token, expires_at: new Date(Date.now() + (answer.json.expires_in ?? 1800) * 1000).toISOString(), stk: stkTag, api };
  writeJsonPrivate(FIRST_PARTY_CACHE, fresh);
  return answer.json.access_token;
}

/** Forgets cached first-party tokens: one identity's, or all (after a pool repair or an Accounts reset). */
export function forgetFirstParty(identity) {
  const cache = identity ? readJson(FIRST_PARTY_CACHE, {}) : {};
  if (identity) delete cache[identity.uuid];
  writeJsonPrivate(FIRST_PARTY_CACHE, cache);
}

/** Headers for /v1/me/… as this identity: the Carbon's browser session, or the Silicon's first-party token. */
export async function accountHeaders(identity, stack = stackFile()) {
  const { site } = stackUrls(stack);
  if (identity.kind === 'carbon') return sessionHeaders(identity, site);
  return { authorization: `Bearer ${await siliconFirstParty(identity, stack)}` };
}

/** A fresh short-lived token for `app` (default dm): works once, for 2 minutes. */
export async function mintSlt(identity, app = 'dm', stack = stackFile()) {
  const { api } = stackUrls(stack);
  let answer = await http('POST', `${api}/v1/me/short-lived-tokens`, { headers: await accountHeaders(identity, stack), json: { app_id: app } });
  if (answer.status === 401 && identity.kind === 'silicon') {
    // The cached sign-in ended (an STK rotation, a sign-out): sign in again once.
    forgetFirstParty(identity);
    answer = await http('POST', `${api}/v1/me/short-lived-tokens`, { headers: await accountHeaders(identity, stack), json: { app_id: app } });
  }
  if (answer.status !== 201 && answer.status !== 200) throw new StackError(`A short-lived token for ${identity.id} at ${app} was refused: ${describe(answer)}`, identity.kind === 'carbon' && answer.status === 401 ? 'The Carbon\'s account-site session ended; repair the pool: node scripts/chat-stack/identities.mjs init' : undefined);
  return answer.json.slt;
}

/** A new sign-in of `identity` at `app` (default dm): the token response, with absolute expiry times added. */
export async function appTokens(identity, app = 'dm', stack = stackFile()) {
  const { api } = stackUrls(stack);
  const slt = await mintSlt(identity, app, stack);
  const answer = await http('POST', `${api}/v1/oauth/token`, {
    basic: [app, appSecret(stack, app)],
    form: { grant_type: 'urn:silicon:params:oauth:grant-type:slt', slt },
  });
  if (answer.status !== 200) throw new StackError(`Exchanging ${identity.id}'s short-lived token at ${app} failed: ${describe(answer)}`);
  const now = Date.now();
  const tokens = answer.json;
  return {
    ...tokens,
    access_expires_at: new Date(now + (tokens.expires_in ?? 1800) * 1000).toISOString(),
    refresh_expires_at: tokens.refresh_token_expires_at ?? new Date(now + 30 * 86400_000).toISOString(),
  };
}

/** A User verification proof that `app` (default dm) acts for `identity` at `receiving` with `scopes`. */
export async function issueProof(identity, { receiving = 'briefcase', scopes, app = 'dm', ttl = 600 }, stack = stackFile()) {
  const { api } = stackUrls(stack);
  const tokens = await appTokens(identity, app, stack);
  const answer = await http('POST', `${api}/v1/proofs/user-verification`, {
    basic: [app, appSecret(stack, app)],
    headers: { 'idempotency-key': `chat-stack-${randomUUID()}` },
    json: { subject_token: tokens.access_token, receiving_app: receiving, scopes, access_ttl_seconds: ttl },
  });
  if (answer.status !== 201 && answer.status !== 200) throw new StackError(`Accounts refused a ${receiving} proof for ${identity.id}: ${describe(answer)}`);
  return answer.json;
}

// ------------------------------------------------------------------------------------------------ the website session

/** Same algorithm as web/lib/server/seal.ts: AES-256-GCM, key = HKDF-SHA256(secret), the purpose as additional data. */
export function seal(value, purpose, secret) {
  const key = Buffer.from(hkdfSync('sha256', secret, 'silicon-web-kit', 'cookie seal v1', 32));
  const iv = randomBytes(12);
  const cipher = createCipheriv('aes-256-gcm', key, iv);
  cipher.setAAD(Buffer.from(purpose));
  const body = Buffer.concat([cipher.update(JSON.stringify(value), 'utf8'), cipher.final()]);
  return `v1.${Buffer.concat([iv, body, cipher.getAuthTag()]).toString('base64url')}`;
}

/** The StoredSession of web/lib/server/session.ts for a fresh DM sign-in of `identity`, sealed as the website's cookie. */
export async function websiteCookie(identity, { secret, secure = false, stack = stackFile() } = {}) {
  const sessionSecret = secret ?? stack.website?.session_secret;
  if (!sessionSecret) throw new StackError('The stack file has no website session secret.', 'Rerun scripts/chat-stack/up.sh (it writes web/.env.local and the secret together).');
  const tokens = await appTokens(identity, 'dm', stack);
  const account = tokens.account ?? {};
  const custodian = account.custodian && typeof account.custodian === 'object' && account.custodian.uuid
    ? { uuid: account.custodian.uuid, id: account.custodian.id } : null;
  const session = {
    v: 1,
    at: tokens.access_token,
    rt: tokens.refresh_token,
    ae: Date.parse(tokens.access_expires_at),
    re: Date.parse(tokens.refresh_expires_at),
    scope: tokens.scope ?? '',
    acct: {
      uuid: account.uuid ?? identity.uuid,
      kind: account.kind ?? identity.kind,
      id: account.id ?? identity.id,
      display_name: (account.display_name || '').trim() || account.id || identity.id,
      pfp_url: account.pfp_url || null,
      custodian,
    },
  };
  const appId = stack.website?.app_id ?? 'dm';
  const base = `${appId.replace(/-/g, '_')}_session`;
  return {
    name: secure ? `__Host-${base}` : base,
    value: seal(session, 'session', sessionSecret),
    expires: Math.floor(session.re / 1000),
    account: session.acct,
  };
}

// ------------------------------------------------------------------------------------------------ the dm CLI

/** The environment that points the dm CLI at the stack as `identity`, in its own home, with the stack's relay. */
export function cliEnv(identity, stack = stackFile(), base = {}) {
  const home = homeOf(identity.id);
  return {
    PATH: `${PATHS.bin}:/usr/bin:/bin:/opt/homebrew/bin`,
    ...base,
    HOME: home,
    SILICON_HOME: home,
    SILICON_DM_RELAY_HOME: PATHS.relay,
    DM_API_URL: stack.dm?.api_url ?? 'http://127.0.0.1:4521',
    ACCOUNTS_URL: stack.accounts?.public_url ?? 'http://localhost:9840',
  };
}

/** The dm binary the helpers run: DM_BIN, else the stack's snapshot (built with the running dm-api). */
export const dmBinary = () => process.env.DM_BIN || join(PATHS.bin, 'dm');

/** One DM API call (contract 4) with a DM access token. */
export async function dmCall(token, method, path, body, { stack = stackFile(), headers = {} } = {}) {
  const url = `${stack.dm?.api_url ?? 'http://127.0.0.1:4521'}${path}`;
  return http(method, url, {
    bearer: token,
    headers: { 'x-dm-contract-version': '4', ...(method === 'GET' ? {} : { 'idempotency-key': `chat-stack-${randomUUID()}` }), ...headers },
    ...(body === undefined ? {} : { json: body }),
  });
}

export const ROOT_DIR = ROOT;
