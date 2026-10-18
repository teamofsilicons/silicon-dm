#!/usr/bin/env node
// The demo workspace of the chat stack: DM as a Carbon sees it on an ordinary morning, instead of test data, for
// screenshots and design reviews. Mira Patel (c:mira) leads the launch of Juniper (a calendar for small studios); Sol
// and Wren are the Silicons she looks after; Bo Okafor (c:dmc-bo, from the pool) designs it with her.
//
//   node scripts/chat-stack/seed-demo.mjs            make what is missing, keep what is there (idempotent)
//   node scripts/chat-stack/seed-demo.mjs --check    say what exists and what is missing; change nothing
//   node scripts/chat-stack/seed-demo.mjs --fresh    a new set: new accounts (c:mira-2, si:sol-2, …; same names), so
//                                                    every conversation starts empty (costs one email code)
//   node scripts/chat-stack/seed-demo.mjs --set 1    back to an earlier set (its accounts, conversations and record)
//   --web <url>   the website the summary links to (default the dev site, http://127.0.0.1:4520)
//
// Everything goes through DM's real API and CLI: the Silicons speak with `dm messages send --speak` (their CLI is
// signed in in their own homes), the files are uploaded into the sender's own Silicon Briefcase (the stand-in) with
// User verification proofs, exactly as web/lib/server/briefcase.ts does for the website, and the pictures are small
// HTML pages rendered by the website's Playwright (lib/demo-assets.mjs). What was made is kept in .dev/demo.json:
// each step's message code and creation time, checked against the service on every run, so a run after a crash goes
// on where it stopped and a run after `up.sh --fresh-dm` makes it all again. DM cannot backdate messages, so every
// message is from the time of the run.
//
// Then: sign a browser in as Mira (node scripts/chat-stack/identities.mjs cookie mira) and look, or run the
// screenshot harness: node scripts/chat-stack/screens.mjs --base <website> --out <folder>.
import { spawnSync } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { DEV, HERE, StackError, describe, http, readJson, sleep, stackFile, writeJsonPrivate } from './lib/common.mjs';
import { appSecret, appTokens, dmCall, findIdentity, issueProof, loadPool, stackUrls, withDemo } from './lib/pool.mjs';
import { renderAssets } from './lib/demo-assets.mjs';

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(`--${name}`);
const option = (name, fallback) => {
  const index = argv.indexOf(`--${name}`);
  return index >= 0 && argv[index + 1] && !argv[index + 1].startsWith('--') ? argv[index + 1] : fallback;
};
const say = (text) => process.stderr.write(`seed-demo: ${text}\n`);
const STATE_FILE = join(DEV, 'demo.json');
const CHECK = flag('check');
const WEB = (option('web', 'http://127.0.0.1:4520')).replace(/\/+$/, '');

/** The voices Mira chooses for her Silicons, so each sounds like itself (two default voices can be the same one). */
const VOICES = { sol: 'mark', wren: 'hope' };

// ------------------------------------------------------------------------------------------------ the stack

const stack = stackFile();
let pool = null;
const who = (name) => findIdentity(pool, name);

const tokens = new Map();
async function tokenOf(name) {
  if (!tokens.has(name)) tokens.set(name, (await appTokens(who(name), 'dm', stack)).access_token);
  return tokens.get(name);
}

/** One DM API call as `name`; answers its `data`, or throws the service's own words. */
async function dm(name, method, path, body, ok = [200, 201, 202, 204]) {
  const answer = await dmCall(await tokenOf(name), method, path, body, { stack });
  if (!ok.includes(answer.status)) throw new StackError(`${method} ${path} as ${who(name).id} failed: ${describe(answer)}`);
  return answer.json?.data ?? null;
}
const enc = encodeURIComponent;

/** The dm CLI as a Silicon (through the stack's relay, like any Silicon); answers the message the service accepted. */
function cli(name, args) {
  const done = spawnSync(process.execPath, [join(HERE, 'identities.mjs'), 'dm', name, '--', '--json', ...args, '--wait'], { encoding: 'utf8', timeout: 90_000 });
  let parsed = null;
  try {
    parsed = JSON.parse(done.stdout.trim().split('\n').pop());
  } catch {
    parsed = null;
  }
  const response = parsed?.response;
  if (done.status !== 0 || response?.state !== 'completed' || !response.result?.['message-id']) {
    const error = response?.error ?? parsed?.error;
    throw new StackError(`dm ${args.slice(0, 2).join(' ')} as ${who(name).id} did not complete (exit ${done.status}): ${error ? `${error.code}: ${error.message}` : (done.stderr || done.stdout).trim().slice(-400)}`,
      'Is the CLI signed in? node scripts/chat-stack/identities.mjs init-demo (it signs the demo Silicons in).');
  }
  return response.result;
}

// ------------------------------------------------------------------------------------------------ uploads

const BRIEFCASE_SCOPES = ['briefcase.uploads.reserve', 'briefcase.uploads.commit', 'briefcase.uploads.status', 'briefcase.uploads.cancel', 'briefcase.link_access.update'];
const encodeSegment = (segment) => encodeURIComponent(segment).replace(/%3A/gi, ':');

/** "20261018-091502-k3f9a2-<name>": Briefcase keeps one file per path (web/lib/server/briefcase.ts storedName). */
function storedName(name) {
  const stamp = new Date().toISOString().replace(/[-:]/g, '').replace('T', '-').slice(0, 15);
  const tag = [...randomBytes(6)].map((byte) => '0123456789abcdefghijklmnopqrstuvwxyz'[byte % 36]).join('');
  return `${stamp}-${tag}-${name}`;
}

/**
 * Saves a file into `owner`'s own Briefcase (the stand-in), under apps/dm/, as DM acting for them: the same proofs,
 * routes and link the website's POST /api/uploads uses (web/lib/server/briefcase.ts publishToBriefcase), so the
 * website shows it through its file proxy. Each Briefcase request gets a proof token never sent before.
 */
async function upload(owner, file) {
  const identity = who(owner);
  const base = stack.briefcase.api_url.replace(/\/+$/, '');
  const { api } = stackUrls(stack);
  const url = new URL(base);
  // DM accepts only https links; the stand-in speaks plain http on this machine, so links say https and the website's
  // proxy maps them back (the website's uploadsSettings publicBase does the same).
  const publicBase = url.protocol === 'http:' ? `https://${url.host}` : base;
  const proof = await issueProof(identity, { receiving: 'briefcase', scopes: BRIEFCASE_SCOPES, ttl: 600 }, stack);
  let unused = proof.proof_token;
  let refresh = proof.proof_refresh_token;
  const basic = ['dm', appSecret(stack, 'dm')];
  const fresh = async () => {
    if (unused) {
      const token = unused;
      unused = null;
      return token;
    }
    const answer = await http('POST', `${api}/v1/proofs/refresh`, { basic, headers: { 'idempotency-key': `refresh-${createHash('sha256').update(refresh).digest('hex').slice(0, 32)}` }, json: { proof_refresh_token: refresh } });
    if (answer.status !== 200) throw new StackError(`Refreshing ${identity.id}'s Briefcase proof failed: ${describe(answer)}`);
    refresh = answer.json.proof_refresh_token;
    return answer.json.proof_token;
  };
  const delegated = async (route, body) => {
    const answer = await http('POST', `${base}/api/v1/obo/${route}`, { headers: { authorization: `Proof ${await fresh()}` }, json: body });
    if (answer.status !== 200 && answer.status !== 201) throw new StackError(`Briefcase ${route} for ${identity.id} failed: ${describe(answer)}`);
    return answer.json;
  };
  try {
    const name = storedName(file.name);
    const operation = randomUUID();
    const reserved = await delegated('uploads/reserve', { operation_id: operation, parent_path: '', name, content_type: file.type, size: file.bytes.length, sha256: file.sha256 });
    const sent = await http('PUT', `${base}/api/v1/obo/uploads/${enc(reserved.upload_id)}/content`, { headers: { 'x-briefcase-upload-capability': reserved.capability, 'content-type': 'application/octet-stream' }, body: file.bytes, timeoutMs: 60_000 });
    if (sent.status !== 200) throw new StackError(`Briefcase transfer for ${identity.id} failed: ${describe(sent)}`);
    const committed = await delegated('uploads/commit', { operation_id: operation, upload_id: reserved.upload_id });
    if (committed.state !== 'committed' || !committed.published_entry_id) throw new StackError(`Briefcase did not publish ${file.name} (it is ${committed.state}).`);
    const link = await delegated('link-access', { operation_id: randomUUID(), entry_id: committed.published_entry_id, enabled: true });
    if (link.effective !== true) throw new StackError(`Briefcase saved ${file.name} but opened no link to it.`);
    let path = ['apps', 'dm', name];
    try {
      const segments = new URL(link.url).pathname.split('/').filter(Boolean).map(decodeURIComponent).slice(1);
      if (segments.length >= 2 && segments.at(-1) === name) path = segments;
    } catch {
      // keep apps/dm/<name>
    }
    return { permanent_url: `${publicBase}/api/v1/public/${encodeSegment(identity.uuid)}/${path.map(encodeSegment).join('/')}?view=inline`, name: file.name, content_type: file.type, size: file.bytes.length };
  } finally {
    await http('POST', `${api}/v1/proofs/revoke`, { basic, json: { proof_id: proof.proof_id } }).catch(() => undefined);
  }
}

/** Whether a link made by upload() still answers at the stand-in (its files live in .dev/briefcase). */
async function stillUploaded(link) {
  const local = link.replace(/^https:/, 'http:').replace(/view=inline$/, 'view=metadata');
  return (await http('GET', local, { timeoutMs: 5000 })).status === 200;
}

// ------------------------------------------------------------------------------------------------ state and steps

let state = null;
const save = () => {
  if (!CHECK) writeJsonPrivate(STATE_FILE, { ...state, updated_at: new Date().toISOString() });
};
const report = [];

/** A file for `owner`, uploaded once per set (kept while the stand-in still has it). */
async function fileFor(owner, key, assets) {
  const asset = assets[key];
  const tag = `${who(owner).uuid}:${asset.sha256}`;
  const kept = state.uploads[tag];
  if (kept && await stillUploaded(kept.permanent_url)) return kept;
  const made = await upload(owner, asset);
  state.uploads[tag] = made;
  save();
  return made;
}

/**
 * A conversation's steps, in order. A step that is recorded and still there (the same message, by its creation time)
 * is kept; from the first one that is not, every later step is made again, so the conversation always reads in order.
 */
class Script {
  constructor(key, address, reader) {
    this.key = key;
    this.address = address;
    this.reader = reader;
    this.record = state.conversations[key] ??= { address, steps: {} };
    this.record.address = address;
    this.missed = false;
    this.made = [];
    this.kept = [];
    this.missing = [];
  }

  async present(done, { edited = false } = {}) {
    if (!done?.code) return false;
    const answer = await dmCall(await tokenOf(this.reader), 'GET', `/api/v1/conversations/${enc(this.address)}/messages/${enc(done.code)}`, undefined, { stack });
    if (answer.status !== 200) return false;
    const message = answer.json?.data;
    return message?.created_at === done.created_at && !message.deleted_at && (!edited || (message.history?.length ?? 0) > 0);
  }

  async step(key, run, options = {}) {
    const done = this.record.steps[key];
    if (!this.missed && await this.present(done, options)) {
      this.kept.push(key);
      return done;
    }
    this.missed = true;
    if (CHECK) {
      this.missing.push(key);
      return done ?? { code: '???', created_at: null };
    }
    const message = await run();
    const result = { code: message['message-id'], created_at: message.created_at, ...(options.edited ? { edited: true } : {}), ...(message.speech ? { spoken: true } : {}) };
    this.record.steps[key] = result;
    save();
    this.made.push(key);
    return result;
  }

  code(key) {
    return this.record.steps[key]?.code;
  }

  summary(extra = {}) {
    const entry = { conversation: this.key, address: this.address, url: `${WEB}/c/${this.address}`, made: this.made, kept: this.kept, ...(CHECK ? { missing: this.missing } : {}), ...extra };
    report.push(entry);
    say(`${this.address.padEnd(28)} ${CHECK ? `${this.kept.length} there, ${this.missing.length} missing` : `${this.made.length} made, ${this.kept.length} kept`}`);
    return entry;
  }
}

/**
 * The direct conversation of `from` with `other`, as the service names it (POST /conversations answers the existing
 * one, or starts it): after `up.sh --fresh-dm` it is started again. --check only looks it up.
 */
async function direct(from, other, key) {
  if (CHECK) {
    const answer = await dmCall(await tokenOf(from), 'GET', `/api/v1/conversations/${enc(who(other).id)}`, undefined, { stack });
    return answer.status === 200 ? answer.json.data.id : state.conversations[key]?.address ?? null;
  }
  return (await dm(from, 'POST', '/api/v1/conversations', { type: 'create_conversation', data: { participant_ids: [who(other).id] } })).id;
}

const send = (from, address, data) => dm(from, 'POST', `/api/v1/conversations/${enc(address)}/messages`, { type: 'message.create', data });
const read = (name, address, upTo, kind = 'read') => (upTo && !CHECK ? dm(name, 'POST', `/api/v1/conversations/${enc(address)}/receipts`, { type: `receipts.${kind}`, data: { up_to: upTo } }) : null);

// ------------------------------------------------------------------------------------------------ the conversations

/** Mira and Sol: the morning summary, a document, code, a reply that was edited, a voice note, read and delivered. */
async function seedSol(assets) {
  const mira = who('mira');
  const address = await direct('mira', 'sol', 'sol');
  const s = new Script('sol', address, 'mira');
  await s.step('sol-morning', () => cli('sol', ['messages', 'send', mira.id, '--speak',
    '[warmly] Morning, Mira. [short pause] Three things today: the launch checklist is green, Bo needs your sign-off on pricing, and standup moved to ten.',
    '--text', 'Your morning in one minute']));
  await s.step('mira-ask', () => send('mira', address, { message: 'Thanks Sol! Can you send me the pricing doc before I sign off on it?' }));
  const pdf = CHECK ? null : await fileFor('sol', 'pricing', assets);
  await s.step('sol-pdf', () => send('sol', address, { message: 'Here it is: pricing v3, with Bo\'s notes on page 2.', attachments: [pdf] }));
  await s.step('sol-query', () => send('sol', address, {
    message: 'And the query behind the trial numbers, in case you want to check them:\n\n```sql\nselect plan,\n       count(*) as trials,\n       round(avg(seats), 1) as avg_seats\nfrom trials\nwhere started_at > now() - interval \'30 days\'\ngroup by plan\norder by trials desc;\n```',
  }));
  const reply = { 'message-id': s.code('sol-pdf') };
  await s.step('mira-reply', () => send('mira', address, { message: 'Page 2 looks rigth. Lets keep the annual discount at 20%.', reply }));
  // The typo is fixed by an edit: the same message, now with its history (the website shows "Edited").
  await s.step('mira-edit', () => dm('mira', 'PATCH', `/api/v1/conversations/${enc(address)}/messages/${enc(s.code('mira-reply'))}`, {
    type: 'message.updated', data: { message: 'Page 2 looks right. Let\'s keep the annual discount at 20%.', reply },
  }), { edited: true });
  await s.step('sol-noted', () => cli('sol', ['messages', 'send', mira.id, '--text', 'Noted. I\'ll update the deck and tell Bo before standup.']));
  await s.step('mira-voice', async () => {
    const words = 'Also, can you move my one-on-one with Bo to Thursday? Friday is packed.';
    const audio = await speechWav('mira', words);
    const now = new Date();
    const pad = (n) => String(n).padStart(2, '0');
    const name = `voice-note-${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}.wav`;
    const note = await upload('mira', { name, type: 'audio/wav', bytes: audio.bytes, sha256: createHash('sha256').update(audio.bytes).digest('hex') });
    return send('mira', address, { voice: { ...note, duration_milliseconds: audio.durationMs }, voice_transcript: words });
  });
  await s.step('sol-done', () => cli('sol', ['messages', 'send', mira.id, '--speak', '[cheerfully] Done. Thursday at three, and Bo has the invite.']));
  await s.step('mira-thanks', () => send('mira', address, { message: 'Perfect, thank you!' }));
  // Sol read everything but the last message, which is only delivered; Mira read all of Sol's.
  await read('sol', address, s.code('mira-voice'));
  await read('sol', address, s.code('mira-thanks'), 'delivered');
  await read('mira', address, s.code('sol-done'));
  return s.summary();
}

/** Mira and Wren: a few messages, one spoken, the last one still unread. */
async function seedWren() {
  const mira = who('mira');
  const address = await direct('mira', 'wren', 'wren');
  const s = new Script('wren', address, 'mira');
  await s.step('wren-build', () => cli('wren', ['messages', 'send', mira.id, '--text', 'Nightly build passed: 1,204 tests, none flaky.']));
  await s.step('mira-cold', () => send('mira', address, { message: 'Nice! Did the cold start get any better?' }));
  await s.step('wren-cold', () => cli('wren', ['messages', 'send', mira.id, '--speak', '[pleased] Yes. Down to one point eight seconds, from two point four. The image cache did most of it.']));
  await s.step('wren-key', () => cli('wren', ['messages', 'send', mira.id, '--text', 'Heads up: the Android build needs a new signing key before Friday. Want me to draft the request?']));
  await read('wren', address, s.code('mira-cold'));
  await read('mira', address, s.code('wren-cold'));
  return s.summary();
}

/** "Launch week": Mira, Bo, Sol and Wren; replies, photos, a GIF, a bundle and a routed message; two unread. */
async function seedGroup(assets) {
  const mira = who('mira');
  const bo = who('bo');
  const sol = who('sol');
  const wren = who('wren');
  const record = state.conversations.group ?? {};
  let group = null;
  if (record.address) {
    const answer = await dmCall(await tokenOf('mira'), 'GET', `/api/v1/groups/${enc(record.address)}`, undefined, { stack });
    if (answer.status === 200 && (answer.json.data.group?.members ?? []).some((member) => member.id === mira.id)) group = answer.json.data;
  }
  if (!group) {
    if (CHECK) {
      say('group "Launch week" is missing');
      report.push({ conversation: 'group', missing: ['the group'] });
      return null;
    }
    group = await dm('mira', 'POST', '/api/v1/groups', { type: 'create_group', data: {
      name: 'Launch week', description: 'Juniper launches on Thursday. The plan, the assets and who does what, in one place.',
      member_ids: [bo.id, sol.id, wren.id],
    } });
    state.conversations.group = { address: group.id, steps: {} };
    save();
    say(`created the group ${group.id}`);
  }
  if (!CHECK) {
    // Bo is a Carbon: he accepts the invitation (Mira's own Silicons joined at once).
    const members = (group.group?.members ?? []).map((member) => member.id);
    if (!members.includes(bo.id)) {
      if (!(group.group?.pending_invitations ?? []).some((member) => member.id === bo.id)) {
        await dm('mira', 'POST', `/api/v1/groups/${enc(group.id)}/members`, { type: 'invite_group_members', data: { member_ids: [bo.id] } });
      }
      await dm('bo', 'POST', `/api/v1/groups/${enc(group.id)}/accept`);
    }
  }
  const address = group.id;
  const s = new Script('group', address, 'mira');
  const photos = CHECK ? {} : {
    heroA: await fileFor('bo', 'heroA', assets), heroB: await fileFor('bo', 'heroB', assets),
    chart: await fileFor('wren', 'chart', assets), whiteboard: await fileFor('bo', 'whiteboard', assets),
  };
  await s.step('g-kickoff', () => send('mira', address, { message: 'Morning, everyone! Juniper launches on Thursday. Let\'s keep everything for it in here.' }));
  await s.step('g-heroes', () => send('bo', address, { message: 'The two hero options from yesterday\'s review. I\'m leaning towards the second.', attachments: [photos.heroA, photos.heroB] }));
  await s.step('g-poll', () => cli('sol', ['messages', 'send', address, '--reply-to', s.code('g-heroes'), '--text', 'The second one won the hallway poll too: seven of nine picked it.']));
  await s.step('g-chart', () => send('wren', address, { message: 'Signups this week against last week.', attachments: [photos.chart] }));
  await s.step('g-spike', () => send('mira', address, { message: 'That Tuesday spike is the newsletter! Let\'s send the next one on launch day.', reply: { 'message-id': s.code('g-chart') } }));
  await s.step('g-whiteboard', () => send('bo', address, { message: 'Run of show from this morning\'s whiteboard. Shout if a time looks wrong.', attachments: [photos.whiteboard] }));
  // Sol keeps the checklist as three messages, then bundles them under one line (only Silicons bundle).
  await s.step('g-check-1', () => send('sol', address, { message: 'Press kit: done.' }));
  await s.step('g-check-2', () => send('sol', address, { message: 'Status page and monitors: done.' }));
  await s.step('g-check-3', () => send('sol', address, { message: 'Support replies for launch day: in review with Bo.' }));
  await s.step('g-bundle', async () => {
    const bundle = await dm('sol', 'POST', `/api/v1/conversations/${enc(address)}/bundles`, { type: 'create_bundle', data: {
      message_ids: [s.code('g-check-1'), s.code('g-check-2'), s.code('g-check-3')],
      display_message: { message: 'Launch checklist: two of three done. Support replies are in review with Bo.' },
    } });
    return bundle.display_message;
  });
  await s.step('g-go', () => send('mira', address, { message: 'Great work, everyone. See you at standup at ten.' }));
  // Wren routes one message to Sol's release notes (ISI routing: `release-notes@si:sol`, from `builds@si:wren`).
  await s.step('g-routed', () => send('wren', address, {
    message: 'Build 1843 is green. The release notes are ready for you to post.', sender_id: `builds@${wren.id}`, recipient_id: `release-notes@${sol.id}`,
  }));
  await s.step('g-gif', async () => {
    const found = await dm('bo', 'GET', `/api/v1/gifs/search?q=${enc('celebrate')}`);
    const gif = found?.items?.[0];
    if (!gif) throw new StackError('The GIF search found nothing for "celebrate".', 'Is the Giphy stand-in up? scripts/chat-stack/status.sh');
    return send('bo', address, { message: 'Thursday energy', gif: { provider_id: gif.provider_id, url: gif.url, ...(gif.preview_url ? { preview_url: gif.preview_url } : {}), ...(gif.title ? { title: gif.title } : {}) } });
  });
  // Everyone else has read it all; Mira read up to her own last message, so the last two are new to her.
  for (const name of ['bo', 'sol', 'wren']) await read(name, address, s.code('g-gif'));
  await read('mira', address, s.code('g-go'));
  return s.summary();
}

/** Mira and Bo: a short Carbon chat (the screenshot harness adds Bo typing). */
async function seedBo() {
  const address = await direct('bo', 'mira', 'bo');
  if (!address) {
    say('the conversation with Bo is missing');
    report.push({ conversation: 'bo', missing: ['the conversation'] });
    return null;
  }
  const s = new Script('bo', address, 'mira');
  await s.step('bo-ask', () => send('bo', address, { message: 'Got a minute before standup? I want to walk you through the pricing page copy.' }));
  await s.step('bo-when', () => send('mira', address, { message: 'Sure, 9:45?' }));
  await s.step('bo-share', () => send('bo', address, { message: 'Perfect. I\'ll share my screen.' }));
  await read('bo', address, s.code('bo-when'));
  await read('mira', address, s.code('bo-share'));
  return s.summary();
}

/** Sol and Wren talk to each other, routed to one intent each: Mira reads it as their custodian (Viewing as). */
async function seedSilicons() {
  const sol = who('sol');
  const wren = who('wren');
  const address = await direct('sol', 'wren', 'silicons');
  if (!address) {
    report.push({ conversation: 'silicons', missing: ['the conversation'] });
    return null;
  }
  const s = new Script('silicons', address, 'sol');
  await s.step('ask', () => send('sol', address, { message: 'Could you pull this week\'s signups for the standup note?', sender_id: `standup@${sol.id}`, recipient_id: `metrics@${wren.id}` }));
  await s.step('answer', () => send('wren', address, { message: 'Sent: 372 signups so far this week, up 48 percent on last week.', sender_id: `metrics@${wren.id}`, recipient_id: `standup@${sol.id}` }));
  await read('wren', address, s.code('ask'));
  await read('sol', address, s.code('answer'));
  return s.summary();
}

/** Bo invites Mira to "Design crit": an invitation waiting for her. */
async function seedInvitation() {
  const mira = who('mira');
  const kept = state.conversations.invitation?.address;
  if (kept) {
    const answer = await dmCall(await tokenOf('bo'), 'GET', `/api/v1/groups/${enc(kept)}`, undefined, { stack });
    const group = answer.json?.data?.group;
    if (answer.status === 200 && ((group?.pending_invitations ?? []).some((x) => x.id === mira.id) || (group?.members ?? []).some((x) => x.id === mira.id))) {
      say(`${kept.padEnd(28)} invitation kept`);
      report.push({ conversation: 'invitation', address: kept, kept: ['invitation'] });
      return kept;
    }
    if (answer.status === 200 && !CHECK) {
      await dm('bo', 'POST', `/api/v1/groups/${enc(kept)}/members`, { type: 'invite_group_members', data: { member_ids: [mira.id] } });
      say(`${kept.padEnd(28)} invited Mira again`);
      report.push({ conversation: 'invitation', address: kept, made: ['invitation'] });
      return kept;
    }
  }
  if (CHECK) {
    report.push({ conversation: 'invitation', missing: ['invitation'] });
    return null;
  }
  const group = await dm('bo', 'POST', '/api/v1/groups', { type: 'create_group', data: {
    name: 'Design crit', description: 'Fridays at four. Bring one thing you are proud of and one you are not.', member_ids: [mira.id],
  } });
  state.conversations.invitation = { address: group.id };
  save();
  say(`${group.id.padEnd(28)} invitation made`);
  report.push({ conversation: 'invitation', address: group.id, made: ['invitation'] });
  return group.id;
}

/** A script rendered in `name`'s own voice (POST /api/v1/speech): the voice note Mira "recorded". */
async function speechWav(name, script) {
  const response = await fetch(`${stack.dm.api_url}/api/v1/speech`, {
    method: 'POST',
    headers: { authorization: `Bearer ${await tokenOf(name)}`, 'content-type': 'application/json', 'x-dm-contract-version': '4' },
    body: JSON.stringify({ type: 'speech.render', data: { script } }),
    signal: AbortSignal.timeout(60_000),
  });
  if (!response.ok) throw new StackError(`Rendering ${who(name).id}'s voice note failed: HTTP ${response.status} ${(await response.text()).slice(0, 300)}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  // 24 kHz mono 16-bit PCM in a RIFF container: the data chunk's size gives the length.
  let durationMs = Number(response.headers.get('x-dm-duration-ms')) || 0;
  for (let offset = 12; !durationMs && offset + 8 <= bytes.length;) {
    const id = bytes.toString('ascii', offset, offset + 4);
    const size = bytes.readUInt32LE(offset + 4);
    if (id === 'data') durationMs = Math.round((size / (bytes.readUInt32LE(28) || 48_000)) * 1000);
    offset += 8 + size + (size % 2);
  }
  return { bytes, durationMs: Math.max(1, durationMs) };
}

/** Waits until every spoken message of the demo has its audio (status ready or failed), so screenshots show waveforms. */
async function speechSettled() {
  const pending = [];
  for (const [key, record] of Object.entries(state.conversations)) {
    for (const done of Object.values(record.steps ?? {})) if (done.spoken) pending.push({ key, address: record.address, code: done.code });
  }
  const deadline = Date.now() + 90_000;
  for (const item of pending) {
    for (;;) {
      const message = await dm('mira', 'GET', `/api/v1/conversations/${enc(item.address)}/messages/${enc(item.code)}`);
      if (message?.speech?.status !== 'pending') break;
      if (Date.now() > deadline) {
        say(`the spoken message ${item.code} in ${item.address} is still being rendered; it will be ready shortly`);
        break;
      }
      await sleep(500);
    }
  }
}

// ------------------------------------------------------------------------------------------------ main

async function main() {
  const ready = await http('GET', `${stack.dm.api_url}/ready`, { timeoutMs: 5000 });
  if (ready.status !== 204) throw new StackError(`dm-api at ${stack.dm.api_url} is not ready (${describe(ready)}).`, 'Start the stack: scripts/chat-stack/up.sh (a restart takes about 10 s; try again then).');
  if (!CHECK) {
    const done = spawnSync(process.execPath, [join(HERE, 'identities.mjs'), 'init-demo', ...(flag('fresh') ? ['--fresh'] : []), ...(option('set') ? ['--set', option('set')] : [])], { encoding: 'utf8', timeout: 10 * 60_000 });
    for (const line of done.stderr.split('\n').filter(Boolean)) process.stderr.write(`${line}\n`);
    if (done.status !== 0) throw new StackError(`The demo accounts could not be made: ${done.stderr.trim().split('\n').slice(-3).join(' | ')}`);
  }
  pool = withDemo(loadPool());
  if (!pool.demo) throw new StackError('There are no demo accounts yet.', 'Run node scripts/chat-stack/seed-demo.mjs (without --check) to make them.');
  // .dev/demo.json describes the current set; another set's record waits in .dev/demo/state-set-<n>.json for --set.
  const kept = readJson(STATE_FILE, null);
  const archive = (set) => join(DEV, 'demo', `state-set-${set}.json`);
  if (kept && kept.set !== pool.demo.set && !CHECK) writeJsonPrivate(archive(kept.set), kept);
  state = kept?.set === pool.demo.set ? kept
    : readJson(archive(pool.demo.set), null) ?? { version: 1, set: pool.demo.set, created_at: new Date().toISOString(), conversations: {}, uploads: {} };
  state.conversations ??= {};
  state.uploads ??= {};
  state.accounts = Object.fromEntries(['mira', 'sol', 'wren', 'bo'].map((name) => [name, { id: who(name).id, uuid: who(name).uuid, display_name: who(name).display_name }]));
  save();
  say(`set ${pool.demo.set}: ${who('mira').display_name} (${who('mira').id}) looks after ${who('sol').display_name} (${who('sol').id}) and ${who('wren').display_name} (${who('wren').id}); Bo is ${who('bo').id}`);

  if (!CHECK) {
    // Mira chooses her Silicons' voices before they speak (a message keeps the voice it was spoken in).
    for (const [name, voice] of Object.entries(VOICES)) {
      await dm('mira', 'PUT', `/api/v1/voices/${enc(who(name).id)}`, { type: 'voice.set', data: { voice } });
    }
  }
  const assets = CHECK ? {} : await renderAssets();
  await seedSol(assets);
  await seedWren();
  await seedGroup(assets);
  await seedBo();
  await seedSilicons();
  await seedInvitation();
  if (!CHECK) await speechSettled();
  state.completed_at = new Date().toISOString();
  save();

  const summary = {
    set: pool.demo.set,
    accounts: state.accounts,
    conversations: report,
    sign_in: 'node scripts/chat-stack/identities.mjs cookie mira',
    state_file: STATE_FILE,
    screenshots: `node scripts/chat-stack/screens.mjs --base ${WEB} --out <folder>`,
  };
  process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
  if (CHECK && report.some((entry) => entry.missing?.length)) process.exitCode = 1;
}

main().catch((error) => {
  process.stderr.write(`error: ${error.message}\n${error.hint ? `hint: ${error.hint}\n` : ''}`);
  if (!(error instanceof StackError) && error.stack) process.stderr.write(`${error.stack.split('\n').slice(1, 4).join('\n')}\n`);
  process.exitCode = 1;
});
