#!/usr/bin/env node
// Proves the chat stack works end to end, part by part (run it after scripts/chat-stack/up.sh):
//
//   node scripts/chat-stack/smoke.mjs [--only dm,cli,deepgram,gifs,briefcase,web] [--keep-web]
//
//   dm        dm-api answers /live and /ready (204)
//   cli       a Silicon signs the dm CLI in with a short-lived token (in its own SILICON_HOME, with the stack's relay),
//             sends a message to its custodian, and the custodian's DM token reads it back through the API
//   deepgram  the stand-in's Voice Agent (key in the Authorization header, as DM's service connects, and as the
//             ['token', key] subprotocol) speaks with the right shape and length, refuses a bad audio setup
//             (INVALID_SETTINGS) and an unknown voice (FAILED_TO_SPEAK); /v1/listen transcribes a WAV and stays quiet
//             for silence; a missing key is 401
//   gifs      dm-api's GIF trending and search answer from the Giphy stand-in
//   briefcase reserve -> transfer -> commit -> link-access with User verification proofs issued by the local Accounts
//             stack for dm (a fresh proof token per request), then the public inline bytes (and a Range) match
//   web       a sealed session cookie opens the website signed in: /auth/session says who, and the workspace page
//             renders without sending the browser to sign in; then the real hosted sign-in in Chromium with only the
//             Carbon's Silicon Accounts session ("Continue as", no email code) lands back on the website signed in
//             (starts `next dev` on 4520 for the check when nothing runs there, and stops it afterwards unless --keep-web)
//
// Prints one line per check and exits 1 when any failed. Never prints a token, key, proof or capability.
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import http from 'node:http';
import { join } from 'node:path';
import { HERE, PATHS, PORTS, ROOT, http as call, portOpen, sleep, stackFile } from './lib/common.mjs';
import { appTokens, cliEnv, dmBinary, dmCall, findIdentity, issueProof, loadPool, stackUrls, websiteCookie } from './lib/pool.mjs';

const argv = process.argv.slice(2);
const onlyIndex = argv.indexOf('--only');
const only = onlyIndex >= 0 ? new Set(argv[onlyIndex + 1].split(',')) : null;
const wants = (name) => !only || only.has(name);
let failures = 0;
const check = (name, ok, detail = '') => {
  if (!ok) failures += 1;
  process.stdout.write(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}\n`);
  return ok;
};

const stack = stackFile();
const pool = loadPool();
const who = (name) => findIdentity(pool, name);

/** A mono 16-bit WAV: a tone (or silence) of `seconds` at `rate`. */
function wav(seconds, { rate = 16000, silent = false } = {}) {
  const samples = Math.round(seconds * rate);
  const data = Buffer.alloc(samples * 2);
  if (!silent) for (let i = 0; i < samples; i++) data.writeInt16LE(Math.round(Math.sin((2 * Math.PI * 220 * i) / rate) * 9000 * (0.5 + 0.5 * Math.sin((Math.PI * i) / rate))), i * 2);
  const header = Buffer.alloc(44);
  header.write('RIFF', 0);
  header.writeUInt32LE(36 + data.length, 4);
  header.write('WAVEfmt ', 8);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20);
  header.writeUInt16LE(1, 22);
  header.writeUInt32LE(rate, 24);
  header.writeUInt32LE(rate * 2, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write('data', 36);
  header.writeUInt32LE(data.length, 40);
  return Buffer.concat([header, data]);
}

// ------------------------------------------------------------------------------------------------ dm and the CLI

async function dm() {
  const live = await call('GET', `${stack.dm.api_url}/live`);
  const ready = await call('GET', `${stack.dm.api_url}/ready`);
  check('dm-api /live and /ready answer 204', live.status === 204 && ready.status === 204, `live ${live.status}, ready ${ready.status}`);
}

function runDm(identity, args, stdin) {
  return new Promise((resolve) => {
    const child = spawn(dmBinary(), args, { env: cliEnv(identity, stack), stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.stdin.end(stdin ?? '');
    child.on('close', (code) => resolve({ code, stdout, stderr }));
  });
}

async function cli() {
  const scout = who('scout');
  const ada = who('ada');
  const { mintSlt } = await import('./lib/pool.mjs');
  const slt = await mintSlt(scout, 'dm', stack);
  const login = await runDm(scout, ['--json', 'login', '--slt-stdin', '--label', 'chat stack smoke'], slt);
  const status = await runDm(scout, ['--json', 'login', 'status']);
  let signedIn = null;
  try {
    signedIn = JSON.parse(status.stdout.trim().split('\n').pop());
  } catch {
    signedIn = null;
  }
  check('a Silicon signs the dm CLI in with a short-lived token, in its own home', login.code === 0 && signedIn?.authenticated === true && signedIn?.id === scout.id,
    `${scout.id} in ${scout.home}; relay ${stack.relay.home}`);
  check('the short-lived token appears in no CLI output', !login.stdout.includes(slt) && !login.stderr.includes(slt));
  const text = `Chat stack smoke ${new Date().toISOString().slice(11, 19)}`;
  const sent = await runDm(scout, ['--json', 'messages', 'send', ada.id, '--text', text, '--wait']);
  let code = null;
  try {
    code = JSON.parse(sent.stdout.trim().split('\n').pop())?.response?.result?.['message-id'] ?? null;
  } catch {
    code = null;
  }
  check('the Silicon sends its custodian a message through the relay', sent.code === 0 && Boolean(code), code ? `message ${code}` : (sent.stderr || sent.stdout).slice(-300));
  const token = (await appTokens(ada, 'dm', stack)).access_token;
  const listed = await dmCall(token, 'GET', `/api/v1/conversations/${encodeURIComponent(`${ada.id}::${scout.id}`)}/messages`, undefined, { stack });
  const found = listed.json?.data?.items?.find((message) => message['message-id'] === code);
  check("the custodian's DM token reads it back through the API", found?.message === text, `GET /api/v1/conversations/${ada.id}::${scout.id}/messages → ${listed.status}`);
}

// ------------------------------------------------------------------------------------------------ Deepgram

const SETTINGS = (voiceId, container = 'none') => ({
  type: 'Settings', mip_opt_out: true, flags: { history: false },
  audio: { input: { encoding: 'linear16', sample_rate: 24000 }, output: { encoding: 'linear16', sample_rate: 24000, container } },
  agent: { speak: { provider: { type: 'eleven_labs', model_id: 'eleven_v4', voice_id: voiceId } } },
});

/** One Voice Agent conversation with Node's WebSocket client (key as the ['token', key] subprotocol). */
function agent(settings, messages) {
  return new Promise((resolve) => {
    const ws = new WebSocket(stack.deepgram.agent_url, ['token', 'chat-stack-smoke']);
    ws.binaryType = 'arraybuffer';
    const events = [];
    let audio = 0;
    const chunks = [];
    const timer = setTimeout(() => finish('timeout'), 15_000);
    function finish(outcome) {
      clearTimeout(timer);
      try {
        ws.close();
      } catch {
        // closing
      }
      resolve({ outcome, events, audio, pcm: Buffer.concat(chunks) });
    }
    ws.onmessage = (event) => {
      if (typeof event.data !== 'string') {
        audio += event.data.byteLength;
        chunks.push(Buffer.from(event.data));
        return;
      }
      const message = JSON.parse(event.data);
      events.push(message);
      if (message.type === 'Welcome') ws.send(JSON.stringify(settings));
      else if (message.type === 'SettingsApplied') for (const text of messages) ws.send(JSON.stringify({ type: 'InjectAgentMessage', message: text, behavior: 'queue' }));
      else if (message.type === 'AgentAudioDone' && events.filter((e) => e.type === 'AgentAudioDone').length === messages.length) finish('done');
      else if (message.type === 'Error') finish(`error:${message.code}`);
    };
    ws.onerror = () => finish('socket_error');
  });
}

/** The upgrade with `Authorization: Token …` (how DM's service connects): returns the status and the first text frame. */
function headerUpgrade(authorization) {
  return new Promise((resolve) => {
    const request = http.request({ host: '127.0.0.1', port: PORTS.deepgram, path: '/v1/agent/converse', headers: {
      connection: 'Upgrade', upgrade: 'websocket', 'sec-websocket-version': '13', 'sec-websocket-key': Buffer.from(randomUUID().slice(0, 16)).toString('base64'),
      ...(authorization ? { authorization } : {}),
    } });
    request.on('upgrade', (response, socket) => {
      socket.once('data', (data) => {
        const length = data[1] & 0x7f;
        const offset = length === 126 ? 4 : 2;
        const size = length === 126 ? data.readUInt16BE(2) : length;
        resolve({ status: response.statusCode, first: data.subarray(offset, offset + size).toString('utf8') });
        socket.destroy();
      });
    });
    request.on('response', (response) => {
      response.resume();
      resolve({ status: response.statusCode, first: null });
    });
    request.on('error', (error) => resolve({ status: 0, first: String(error) }));
    request.end();
  });
}

function peaks(pcm, count = 64) {
  const samples = pcm.length / 2;
  const window = Math.max(1, Math.floor(samples / count));
  const out = [];
  for (let w = 0; w < count; w++) {
    let sum = 0;
    for (let i = w * window; i < Math.min(samples, (w + 1) * window); i++) sum += (pcm.readInt16LE(i * 2) / 32768) ** 2;
    out.push(Math.sqrt(sum / window));
  }
  return out;
}

async function deepgram() {
  const header = await headerUpgrade('Token chat-stack-smoke');
  check('Voice Agent accepts the key in the Authorization header and says Welcome first', header.status === 101 && JSON.parse(header.first || '{}').type === 'Welcome', `HTTP ${header.status}`);
  const refused = await headerUpgrade(null);
  check('Voice Agent refuses a connection without a key (401)', refused.status === 401, `HTTP ${refused.status}`);
  const text = '[excited] It works! The stand-in speaks.';
  const spoken = text.replace(/\[[^\]]*\]/g, ' ').replace(/\s+/g, ' ').trim();
  const one = await agent(SETTINGS('UgBBYS2sOqTuMpoF3BR0'), [text]);
  const order = one.events.map((event) => event.type).join(' → ');
  const expected = Math.round(24000 * (spoken.length * 65) / 1000) * 2;
  const levels = peaks(one.pcm);
  const varied = Math.max(...levels) > 0.05 && Math.min(...levels) < Math.max(...levels) * 0.6;
  check('Voice Agent: Welcome → SettingsApplied → ConversationText(assistant) → audio → AgentAudioDone', one.outcome === 'done'
    && order.startsWith('Welcome → SettingsApplied → ConversationText') && one.events.find((e) => e.type === 'ConversationText')?.role === 'assistant', order);
  check('speech audio is linear16 24 kHz sized like real speech (~65 ms per spoken character), with a varied waveform', Math.abs(one.audio - expected) <= 4 && varied,
    `${one.audio} bytes = ${(one.audio / 48000).toFixed(2)} s for ${spoken.length} characters`);
  const two = await agent(SETTINGS('kPzsL2i3teMYv0FxEYQ6'), ['First sentence.', 'Second sentence, queued.']);
  check('two queued injections are spoken in order (two ConversationText, two AgentAudioDone)', two.outcome === 'done'
    && two.events.filter((e) => e.type === 'ConversationText').map((e) => e.content).join('|') === 'First sentence.|Second sentence, queued.');
  const container = await agent(SETTINGS('UgBBYS2sOqTuMpoF3BR0', 'wav'), ['Hello.']);
  check('a bad audio setup (container wav) is Error INVALID_SETTINGS', container.outcome === 'error:INVALID_SETTINGS', container.outcome);
  const unknown = await agent(SETTINGS('JBFqnCBsd6RMkjVDRZzb'), ['Hello.']);
  check('a voice_id outside the five is Error FAILED_TO_SPEAK', unknown.outcome === 'error:FAILED_TO_SPEAK', unknown.outcome);
  const cuesOnly = await agent(SETTINGS('UgBBYS2sOqTuMpoF3BR0'), ['[laughs]']);
  check('a script of cues only is Error FAILED_TO_SPEAK, as with the real Voice Agent', cuesOnly.outcome === 'error:FAILED_TO_SPEAK', cuesOnly.outcome);
  const paused = await agent(SETTINGS('tnSpp4vdxKPjI9w0GnoV'), ['Wait [long pause] now.']);
  const pausedMs = (paused.audio / 48);
  check('a pause cue adds silence instead of being read ([long pause] about 1.7 s)', paused.outcome === 'done' && Math.abs(pausedMs - (8 * 65 + 1700)) < 5, `${Math.round(pausedMs)} ms`);

  const listen = (body, type = 'audio/wav', auth = 'Token chat-stack-smoke') => call('POST', `${stack.deepgram.listen_url}?model=nova-3&smart_format=true&punctuate=true&mip_opt_out=true&language=en`,
    { headers: { 'content-type': type, ...(auth ? { authorization: auth } : {}) }, body });
  const note = await listen(wav(2.4));
  const alternative = note.json?.results?.channels?.[0]?.alternatives?.[0];
  check('/v1/listen answers Deepgram\'s shape with a deterministic transcript and metadata.duration', note.status === 200 && alternative?.transcript === 'This is a voice note from the stand-in.'
    && Math.abs(note.json.metadata.duration - 2.4) < 0.01 && note.headers.get('x-dm-stand-in') === 'deepgram', `"${alternative?.transcript}", ${note.json?.metadata?.duration} s`);
  const long = await listen(wav(9.6));
  check('a longer note names its length', long.json?.results?.channels?.[0]?.alternatives?.[0]?.transcript === 'This is a voice note from the stand-in. It is 10 seconds long.', long.json?.results?.channels?.[0]?.alternatives?.[0]?.transcript);
  const silence = await listen(wav(1.5, { silent: true }));
  check('silence transcribes to an empty transcript (200)', silence.status === 200 && silence.json?.results?.channels?.[0]?.alternatives?.[0]?.transcript === '');
  const anonymous = await listen(wav(1), 'audio/wav', null);
  check('/v1/listen without a key is 401 INVALID_AUTH', anonymous.status === 401 && anonymous.json?.err_code === 'INVALID_AUTH', `HTTP ${anonymous.status}`);
}

// ------------------------------------------------------------------------------------------------ GIFs

async function gifs() {
  const token = (await appTokens(who('bo'), 'dm', stack)).access_token;
  const trending = await dmCall(token, 'GET', '/api/v1/gifs/trending', undefined, { stack });
  const items = trending.json?.data?.items ?? [];
  check('dm-api answers GIF trending from the Giphy stand-in (https media from media.giphy.com)', trending.status === 200 && items.length === 6 && items.every((gif) => gif.url.startsWith('https://media.giphy.com/')), `HTTP ${trending.status}, ${items.length} GIFs`);
  const hello = await dmCall(token, 'GET', '/api/v1/gifs/search?q=hello', undefined, { stack });
  const none = await dmCall(token, 'GET', '/api/v1/gifs/search?q=zzz', undefined, { stack });
  check('GIF search finds by word and answers an empty list for an unknown word', hello.json?.data?.items?.[0]?.provider_id === 'xT9IgG50Fb7Mi0prBC' && none.status === 200 && none.json?.data?.items?.length === 0,
    `hello → ${hello.json?.data?.items?.length}, zzz → ${none.json?.data?.items?.length}`);
}

// ------------------------------------------------------------------------------------------------ Briefcase

async function briefcase() {
  const ada = who('ada');
  const { api } = stackUrls(stack);
  const base = stack.briefcase.api_base_url;
  const scopes = ['briefcase.uploads.reserve', 'briefcase.uploads.commit', 'briefcase.uploads.status', 'briefcase.uploads.cancel', 'briefcase.link_access.update'];
  const proof = await issueProof(ada, { scopes, ttl: 600 }, stack);
  check('the local Accounts stack issues dm a User verification proof for briefcase', proof.receiving_app === 'briefcase' && proof.issuing_app === 'dm' && proof.user?.id === ada.id, `proof ${proof.proof_id}`);
  // A fresh proof token for every Briefcase request (spec §6.9): refresh the proof, keep the newest refresh token.
  let refreshToken = proof.proof_refresh_token;
  let first = proof.proof_token;
  const fresh = async () => {
    if (first) {
      const token = first;
      first = null;
      return token;
    }
    const answer = await call('POST', `${api}/v1/proofs/refresh`, {
      basic: ['dm', stack.apps.dm.app_secret], headers: { 'idempotency-key': `refresh-${createHash('sha256').update(refreshToken).digest('hex').slice(0, 32)}` },
      json: { proof_refresh_token: refreshToken, access_ttl_seconds: 300 },
    });
    if (answer.status !== 200) throw new Error(`refreshing the proof answered ${answer.status}`);
    refreshToken = answer.json.proof_refresh_token;
    return answer.json.proof_token;
  };
  const bytes = wav(1.2);
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  const name = `smoke-${Date.now()}.wav`;
  const operation = randomUUID();
  const manifest = { operation_id: operation, parent_path: '', name, content_type: 'audio/wav', size: bytes.length, sha256 };
  const anonymous = await call('POST', `${base}/obo/uploads/reserve`, { json: manifest });
  check('reserve without a proof is 401 authentication_required', anonymous.status === 401 && anonymous.json?.error?.code === 'authentication_required', `HTTP ${anonymous.status}`);
  const reserved = await call('POST', `${base}/obo/uploads/reserve`, { headers: { authorization: `Proof ${await fresh()}` }, json: manifest });
  check('reserve answers a reservation with a capability', reserved.status === 200 && reserved.json?.state === 'reserved' && typeof reserved.json?.capability === 'string', `HTTP ${reserved.status} ${reserved.json?.state ?? reserved.json?.error?.code}`);
  const transferred = await call('PUT', `${base}/obo/uploads/${reserved.json.upload_id}/content`, {
    headers: { 'x-briefcase-upload-capability': reserved.json.capability, 'content-type': 'application/octet-stream', 'content-length': String(bytes.length) }, body: bytes,
  });
  check('the bytes transfer with the capability alone (staged)', transferred.status === 200 && transferred.json?.state === 'staged', `HTTP ${transferred.status} ${transferred.json?.state ?? transferred.json?.error?.code}`);
  const committed = await call('POST', `${base}/obo/uploads/commit`, { headers: { authorization: `Proof ${await fresh()}` }, json: { operation_id: operation, upload_id: reserved.json.upload_id } });
  check('commit with a fresh proof publishes the file', committed.status === 200 && committed.json?.state === 'committed' && Boolean(committed.json?.published_entry_id), `HTTP ${committed.status} ${committed.json?.state ?? committed.json?.error?.code}`);
  const linked = await call('POST', `${base}/obo/link-access`, { headers: { authorization: `Proof ${await fresh()}` }, json: { operation_id: randomUUID(), entry_id: committed.json.published_entry_id, enabled: true } });
  check('link-access turns on "anyone with the link" and answers the permanent URL', linked.status === 200 && linked.json?.effective === true && String(linked.json?.url).endsWith(`/${ada.id}/apps/dm/${name}`), linked.json?.url ?? linked.json?.error?.code);
  const publicUrl = `${base}/public/${ada.id}/apps/dm/${encodeURIComponent(name)}?view=inline`;
  const inline = await fetch(publicUrl);
  const served = Buffer.from(await inline.arrayBuffer());
  check('the public inline view serves the exact bytes as audio/wav (with Briefcase\'s isolation headers)', inline.status === 200 && served.equals(bytes) && inline.headers.get('content-type') === 'audio/wav'
    && inline.headers.get('cross-origin-resource-policy') === 'same-origin', `${publicUrl} → ${inline.status}`);
  const ranged = await fetch(publicUrl, { headers: { range: 'bytes=0-43' } });
  check('a Range request answers 206 with the WAV header', ranged.status === 206 && Buffer.from(await ranged.arrayBuffer()).subarray(0, 4).toString() === 'RIFF', `HTTP ${ranged.status} ${ranged.headers.get('content-range')}`);
  const narrow = await issueProof(ada, { scopes: ['briefcase.uploads.status'], ttl: 120 }, stack);
  const missing = await call('POST', `${base}/obo/uploads/reserve`, { headers: { authorization: `Proof ${narrow.proof_token}` }, json: { ...manifest, operation_id: randomUUID() } });
  check('a proof without the route\'s scope is 403 proof_scope_missing', missing.status === 403 && missing.json?.error?.code === 'proof_scope_missing', `HTTP ${missing.status} ${missing.json?.error?.code}`);
  await call('POST', `${api}/v1/proofs/revoke`, { basic: ['dm', stack.apps.dm.app_secret], json: { proof_id: proof.proof_id } });
  await call('POST', `${api}/v1/proofs/revoke`, { basic: ['dm', stack.apps.dm.app_secret], json: { proof_id: narrow.proof_id } });
}

// ------------------------------------------------------------------------------------------------ the website

async function web() {
  const url = stack.website.dev_url;
  let child = null;
  if (!(await portOpen(PORTS.web_dev))) {
    process.stdout.write(`.... starting next dev on ${url} for the check\n`);
    child = spawn(join(ROOT, 'web', 'node_modules', '.bin', 'next'), ['dev', '--port', String(PORTS.web_dev)], {
      cwd: join(ROOT, 'web'), env: { PATH: process.env.PATH, HOME: process.env.HOME, NEXT_TELEMETRY_DISABLED: '1' }, stdio: ['ignore', 'ignore', 'pipe'], detached: true,
    });
    let stderr = '';
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    for (let i = 0; i < 180 && !(await portOpen(PORTS.web_dev)); i++) await sleep(1000);
    if (!(await portOpen(PORTS.web_dev))) {
      check('the dev website starts on 4520', false, stderr.slice(-400));
      return;
    }
  }
  try {
    const ada = who('ada');
    const cookie = await websiteCookie(ada, { stack });
    const header = { cookie: `${cookie.name}=${cookie.value}` };
    const session = await call('GET', `${url}/auth/session`, { headers: header, timeoutMs: 180_000 });
    check('a sealed session cookie is signed in on the website (/auth/session)', session.json?.signed_in === true && session.json?.account?.id === ada.id, `${session.status} ${JSON.stringify(session.json?.account?.id ?? session.json)}`);
    const signedOut = await call('GET', `${url}/auth/session`, { timeoutMs: 60_000 });
    check('without the cookie the website says signed out', signedOut.json?.signed_in === false, `${signedOut.status}`);
    let page = await call('GET', `${url}/c`, { headers: { ...header, accept: 'text/html' }, timeoutMs: 180_000 });
    let path = '/c';
    if (page.status === 404) {
      path = '/conversations';
      page = await call('GET', `${url}${path}`, { headers: { ...header, accept: 'text/html' }, timeoutMs: 180_000 });
    }
    const location = page.headers.get('location') ?? '';
    const named = page.text.includes(ada.display_name) || page.text.includes(ada.id);
    check(`the workspace (${path}) renders signed in as the cookie's account`, page.status === 200 && !location.includes('sign-in') && named, `HTTP ${page.status}${location ? ` → ${location}` : ''}${named ? `, shows ${ada.display_name}` : ', the account is not on the page'}`);
    const anonymous = await call('GET', `${url}${path}`, { headers: { accept: 'text/html' }, timeoutMs: 60_000 });
    check('without the cookie the workspace sends the browser to sign in', anonymous.status >= 300 && anonymous.status < 400 && /sign-in/.test(anonymous.headers.get('location') ?? ''), `HTTP ${anonymous.status} → ${anonymous.headers.get('location')}`);
    const otherPort = await call('GET', `${url.replace('127.0.0.1', 'localhost')}/auth/session`, { headers: header, timeoutMs: 60_000 });
    check('the cookie is accepted whichever loopback name the website is opened with (localhost or 127.0.0.1)', otherPort.json?.signed_in === true, `${otherPort.status}`);
    // The Playwright build (4530) reads the same SESSION_SECRET, so the same cookie must open it too (when it runs).
    if (await portOpen(PORTS.web_e2e)) {
      const e2e = await call('GET', `${stack.website.e2e_url}/auth/session`, { headers: header, timeoutMs: 60_000 });
      check(`the same cookie is signed in on the e2e website (${stack.website.e2e_url})`, e2e.json?.signed_in === true && e2e.json?.account?.id === ada.id, `${e2e.status}`);
    }
    await hostedSignIn(url, ada);
  } finally {
    if (child && !argv.includes('--keep-web')) {
      try {
        process.kill(-child.pid, 'SIGTERM');
      } catch {
        child.kill('SIGTERM');
      }
      process.stdout.write('.... stopped the dev website again\n');
    }
  }
}

/**
 * The real sign-in through the hosted pages, in Chromium: the browser holds only Ada's Silicon Accounts session (from
 * `identities.mjs storage-state`), the website sends it to the account site, which offers "Continue as" (no email
 * code), shows DM's details page, and returns to {website}/auth/callback, a redirect URI the stack registered.
 */
async function hostedSignIn(url, ada) {
  let chromium;
  try {
    ({ chromium } = await import(join(ROOT, 'web', 'node_modules', '@playwright', 'test', 'index.mjs')));
  } catch {
    check('hosted sign-in (skipped: install the website first, pnpm -C web install)', true);
    return;
  }
  const { execFileSync } = await import('node:child_process');
  const state = JSON.parse(execFileSync(process.execPath, [join(HERE, 'identities.mjs'), 'storage-state', 'ada'], { encoding: 'utf8' }));
  state.cookies = state.cookies.filter((cookie) => cookie.name !== 'dm_session');
  const browser = await chromium.launch();
  try {
    const context = await browser.newContext({ storageState: state });
    const page = await context.newPage();
    await page.goto(`${url}/c`, { timeout: 180_000 });
    await page.getByRole('button', { name: /^Continue as / }).click({ timeout: 180_000 });
    const share = page.getByRole('button', { name: 'Share and continue' });
    const back = (target) => target.href.startsWith(url) && !target.pathname.startsWith('/auth/');
    const stop = await Promise.race([
      page.waitForURL(back, { timeout: 120_000 }).then(() => 'website'),
      share.waitFor({ timeout: 120_000 }).then(() => 'details'),
    ]);
    if (stop === 'details') await share.click();
    await page.waitForURL(back, { timeout: 120_000 });
    const session = await (await context.request.get(`${url}/auth/session`)).json();
    check('the hosted sign-in ("Continue as", no email code) returns to the website signed in', session.signed_in === true && session.account?.id === ada.id, `landed on ${new URL(page.url()).pathname}`);
  } catch (error) {
    check('the hosted sign-in ("Continue as", no email code) returns to the website signed in', false, error.message.split('\n')[0]);
  } finally {
    await browser.close();
  }
}

const parts = { dm, cli, deepgram, gifs, briefcase, web };
for (const [name, run] of Object.entries(parts)) {
  if (!wants(name)) continue;
  try {
    await run();
  } catch (error) {
    check(`${name}: ${error.message}`, false, error.hint ?? '');
  }
}
process.stdout.write(`${failures ? `${failures} check(s) failed` : 'every check passed'} (stack file ${PATHS.stack}; helpers in ${HERE})\n`);
process.exitCode = failures ? 1 : 0;
