#!/usr/bin/env node
// The Briefcase stand-in of the chat stack (127.0.0.1:4541): Silicon Briefcase's delegated uploads (Briefcase 4,
// `openapi.yaml` and `docs/obo.md` on silicon-briefcase's main branch), with the same request and response shapes,
// error codes and headers, so the DM website can save files and voice notes into a Carbon's own Briefcase locally.
//
//   POST /api/v1/obo/uploads/reserve   {operation_id, parent_path, name, content_type, size, sha256}
//        -> {operation_id, upload_id, state:"reserved", expires_at, published_entry_id:null, capability}
//   PUT  /api/v1/obo/uploads/{upload_id}/content   X-Briefcase-Upload-Capability, exact Content-Length, no
//        Authorization, no chunked encoding; the SHA-256 must match -> {…, state:"staged"}
//   POST /api/v1/obo/uploads/commit    {operation_id, upload_id} -> {…, state:"committed", published_entry_id}
//   POST /api/v1/obo/uploads/status    {operation_id}            POST /api/v1/obo/uploads/cancel {operation_id}
//   POST /api/v1/obo/folders/create    {operation_id, parent_path, name}
//   POST /api/v1/obo/link-access       {operation_id, entry_id, enabled, expires_in_minutes?}
//        -> {url:"http://127.0.0.1:4541/{owner id}/{path}", enabled, effective, inherited_from, can_manage, expires_at}
//   GET  /api/v1/public/{owner}/{path}?view=metadata|contents|inline|attachment   what anyone with the link may read
//        (Range supported; the headers Briefcase sends, including Cross-Origin-Resource-Policy: same-origin, so a page
//        on another origin cannot embed the bytes directly: the DM website proxies them, as Briefcase's own site does)
//   GET  /{owner}/{path}                -> 302 to the inline view (the "permanent URL" of a linked file)
//
// Every route under /api/v1/obo/ except the byte transfer needs `Authorization: Proof sap_…`: a User verification
// proof whose receiving app is briefcase. Each one is verified with the LOCAL Silicon Accounts stack
// (POST /v1/proofs/verify with briefcase's app credentials) and must carry the route's scope, from an issuer
// BRIEFCASE_PROOF_ISSUERS allows (default: dm, for every delegated scope). As in Briefcase, an ordinary issuer reaches
// only apps/{issuer}/ in the account's own drive; parent_path "" is that folder.
//
// Files live in BRIEFCASE_STANDIN_DIR (default .dev/briefcase): state.json, blobs/<sha256>, staging/<upload_id>.
// Test controls: GET /_health, GET /_state, GET|DELETE /_requests,
//   POST /_faults {"route":"reserve"|"transfer"|"commit"|"status"|"cancel"|"link-access"|"public","status":503,"code":"…","count":1}
// Requests are logged (method, path, status, sizes; never bodies, proofs or capabilities) to BRIEFCASE_STANDIN_LOG.
import { createHash, randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import { appendFileSync, createReadStream, createWriteStream, existsSync, mkdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { dirname, join, resolve } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { Transform } from 'node:stream';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, '..', '..');
const portIndex = process.argv.indexOf('--port');
const PORT = Number(portIndex >= 0 ? process.argv[portIndex + 1] : process.env.BRIEFCASE_STANDIN_PORT || 4541);
const DIR = process.env.BRIEFCASE_STANDIN_DIR || join(ROOT, '.dev', 'briefcase');
const LOG = process.env.BRIEFCASE_STANDIN_LOG || join(ROOT, '.dev', 'logs', 'briefcase.log');
const ACCOUNTS_API_URL = (process.env.ACCOUNTS_API_URL || '').replace(/\/+$/, '');
const APP_ID = process.env.BRIEFCASE_APP_ID || 'briefcase';
const APP_SECRET = process.env.BRIEFCASE_APP_SECRET || '';
const SITE = (process.env.BRIEFCASE_PUBLIC_SITE_URL || `http://127.0.0.1:${PORT}`).replace(/\/+$/, '');
const MAX_UPLOAD_BYTES = Number(process.env.BRIEFCASE_STANDIN_MAX_BYTES || 5 * 1024 ** 3);
const RESERVATION_TTL_MS = 24 * 3600_000;
const SCOPES = ['briefcase.folders.create', 'briefcase.entries.list', 'briefcase.files.read', 'briefcase.entries.trash', 'briefcase.uploads.reserve',
  'briefcase.uploads.commit', 'briefcase.uploads.status', 'briefcase.uploads.cancel', 'briefcase.invitations.create', 'briefcase.link_access.update'];

for (const [name, value] of [['ACCOUNTS_API_URL', ACCOUNTS_API_URL], ['BRIEFCASE_APP_SECRET', APP_SECRET]]) {
  if (!value) {
    process.stderr.write(`briefcase stand-in: ${name} is not set (scripts/chat-stack/up.sh sets it from the stack)\n`);
    process.exit(2);
  }
}
if (!['localhost', '127.0.0.1', '[::1]'].includes(new URL(ACCOUNTS_API_URL).hostname)) {
  process.stderr.write(`briefcase stand-in: ACCOUNTS_API_URL ${ACCOUNTS_API_URL} is not on this machine; the stand-in only verifies proofs with a local stack\n`);
  process.exit(2);
}

/** BRIEFCASE_PROOF_ISSUERS, Briefcase's own format: `scope=app|app`, separated by commas, semicolons or spaces. */
function parseIssuers(value) {
  const map = new Map();
  for (const entry of value.split(/[,;\s]+/).filter(Boolean)) {
    const [scope, apps] = entry.split('=');
    if (!scope || !apps) throw new Error(`BRIEFCASE_PROOF_ISSUERS entry "${entry}" is not scope=app|app`);
    map.set(scope, new Set(apps.split('|').filter(Boolean)));
  }
  return map;
}
const ISSUERS = parseIssuers(process.env.BRIEFCASE_PROOF_ISSUERS || SCOPES.map((scope) => `${scope}=dm`).join(','));
const FIRST_PARTY = new Set((process.env.BRIEFCASE_PROOF_FIRST_PARTY_ISSUERS || '').split(/[,;\s]+/).filter(Boolean));

// ------------------------------------------------------------------------------------------------ state

mkdirSync(join(DIR, 'blobs'), { recursive: true, mode: 0o700 });
mkdirSync(join(DIR, 'staging'), { recursive: true, mode: 0o700 });
const STATE_FILE = join(DIR, 'state.json');
const state = existsSync(STATE_FILE) ? JSON.parse(readFileSync(STATE_FILE, 'utf8')) : { accounts: {}, entries: {}, uploads: {}, operations: {}, mutations: {} };
for (const key of ['accounts', 'entries', 'uploads', 'operations', 'mutations']) state[key] ??= {};
function save() {
  const temporary = `${STATE_FILE}.${process.pid}.tmp`;
  writeFileSync(temporary, JSON.stringify(state, null, 1), { mode: 0o600 });
  renameSync(temporary, STATE_FILE);
}

const recent = [];
function record(entry) {
  const line = { at: new Date().toISOString(), ...entry };
  recent.unshift(line);
  if (recent.length > 300) recent.length = 300;
  try {
    mkdirSync(dirname(LOG), { recursive: true });
    appendFileSync(LOG, `${JSON.stringify(line)}\n`, { mode: 0o600 });
  } catch {
    // logging never breaks an answer
  }
}

let faults = [];
function takeFault(route) {
  const index = faults.findIndex((fault) => fault.route === route && fault.remaining > 0);
  if (index < 0) return null;
  const fault = faults[index];
  fault.remaining -= 1;
  if (fault.remaining <= 0) faults.splice(index, 1);
  return fault;
}

// ------------------------------------------------------------------------------------------------ answers

class Refusal extends Error {
  constructor(status, code, message, headers = {}) {
    super(message);
    this.status = status;
    this.code = code;
    this.headers = headers;
  }
}
const MESSAGES = {
  400: 'The request could not be interpreted.',
  404: 'The requested resource was not found.',
  409: 'The request conflicts with the current resource state.',
  422: 'The request contains invalid data.',
};
const refuse = (status, code, message) => new Refusal(status, code, message ?? MESSAGES[status] ?? code);

function sendJson(res, status, value, headers = {}) {
  const body = JSON.stringify(value);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body), 'x-dm-stand-in': 'briefcase', ...headers });
  res.end(body);
}

function sendError(res, error, requestId) {
  sendJson(res, error.status, { error: { code: error.code, message: error.message, request_id: requestId } }, error.headers);
}

async function readJsonBody(req, limit = 1024 * 1024) {
  let size = 0;
  const chunks = [];
  for await (const chunk of req) {
    size += chunk.length;
    if (size > limit) throw refuse(413, 'payload_too_large', 'The request body exceeds the allowed size.');
    chunks.push(chunk);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw refuse(400, 'invalid_json');
  }
}

/** serde's deny_unknown_fields + required fields + types: a wrong body is 400 invalid_json, as in Briefcase. */
function shape(body, fields) {
  if (!body || typeof body !== 'object' || Array.isArray(body)) throw refuse(400, 'invalid_json');
  for (const key of Object.keys(body)) if (!(key in fields)) throw refuse(400, 'invalid_json');
  for (const [key, type] of Object.entries(fields)) {
    const optional = type.endsWith('?');
    const kind = type.replace('?', '');
    const value = body[key];
    if (value === undefined || value === null) {
      if (optional) continue;
      throw refuse(400, 'invalid_json');
    }
    const ok = kind === 'uuid' ? typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)
      : kind === 'u64' ? Number.isInteger(value) && value >= 0
        : typeof value === kind;
    if (!ok) throw refuse(400, 'invalid_json');
  }
  return body;
}

const NIL = '00000000-0000-0000-0000-000000000000';
function requireOperationId(id) {
  if (id.toLowerCase() === NIL) throw refuse(422, 'invalid_operation_id');
}

// ------------------------------------------------------------------------------------------------ proofs

async function authorizeProof(req, scope) {
  const header = req.headers.authorization;
  if (!header) throw refuse(401, 'authentication_required', 'Send Authorization: Bearer <access token> with a Silicon Accounts access token issued to Briefcase.');
  const parts = String(header).trim().split(/\s+/);
  if (parts.length !== 2) throw refuse(401, 'authorization_malformed', 'The Authorization header must be one scheme and one credential.');
  if (parts[0].toLowerCase() !== 'proof') {
    throw refuse(401, 'proof_required', 'This route is for apps acting for an account: send Authorization: Proof sap_… with a Silicon Accounts User verification proof whose receiving app is briefcase.');
  }
  const token = parts[1];
  if (token.length > 512 || !/^sap_[A-Za-z0-9_-]+$/.test(token)) {
    throw refuse(401, 'proof_malformed', 'The proof is not a Silicon Accounts proof token (sap_…). Send the proof_token, not the proof_refresh_token (sapr_…).');
  }
  let verification;
  try {
    const answer = await fetch(`${ACCOUNTS_API_URL}/v1/proofs/verify`, {
      method: 'POST',
      headers: { authorization: `Basic ${Buffer.from(`${APP_ID}:${APP_SECRET}`).toString('base64')}`, 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify({ proof_token: token }),
      signal: AbortSignal.timeout(10_000),
    });
    if (!answer.ok) throw new Error(`HTTP ${answer.status}`);
    verification = await answer.json();
  } catch (error) {
    throw refuse(503, 'dependency_unavailable', `A required service is temporarily unavailable (Silicon Accounts did not verify the proof: ${error.message}).`);
  }
  if (verification.valid !== true || verification.receiving_app?.app_id !== APP_ID) {
    throw refuse(401, 'proof_invalid', "The proof is not valid for Briefcase right now: it is unknown, expired or revoked, it was issued for another app, or the account's sign-in at the issuing app ended. Ask the issuing app for a new proof.");
  }
  const issuer = verification.issuing_app?.app_id;
  if (verification.kind !== 'user_verification') {
    throw refuse(403, 'proof_kind_not_accepted', `Briefcase acts only for an account, but ${issuer} sent an App verification proof. Send a User verification proof for the account.`);
  }
  if (!(verification.scopes ?? []).includes(scope)) {
    throw refuse(403, 'proof_scope_missing', `The proof from ${issuer} does not carry the scope \`${scope}\` this route requires. Ask for a proof with that scope.`);
  }
  if (!ISSUERS.get(scope)?.has(issuer)) {
    throw refuse(403, 'proof_issuer_not_allowed', `${issuer} may not use \`${scope}\` at Briefcase. Briefcase's operator lists the apps allowed for each scope in BRIEFCASE_PROOF_ISSUERS.`);
  }
  const user = verification.user;
  if (!user?.uuid) throw refuse(401, 'proof_invalid', 'The proof names no account.');
  state.accounts[user.uuid] = { uuid: user.uuid, id: user.id ?? state.accounts[user.uuid]?.id ?? null, kind: user.kind ?? null, seen_at: new Date().toISOString() };
  return { owner: user.uuid, ownerId: user.id, issuer, firstParty: FIRST_PARTY.has(issuer), proofId: verification.proof_id };
}

// ------------------------------------------------------------------------------------------------ paths and entries

function entryName(name) {
  if (typeof name !== 'string' || !name.length || [...name].length > 255 || name.includes('/') || name === '.' || name === '..' || /[\u0000-\u001f\u007f]/.test(name) || name.trim() !== name) {
    throw refuse(422, 'invalid_name');
  }
  return name;
}

function entryPath(path) {
  const segments = String(path).replace(/^\/+|\/+$/g, '').split('/');
  if (!segments.length || segments.some((segment) => !segment || segment === '.' || segment === '..' || /[\u0000-\u001f\u007f]/.test(segment))) return null;
  return segments.join('/');
}

const findEntry = (owner, path) => Object.values(state.entries).find((entry) => entry.owner === owner && entry.path === path && !entry.deleted_at) ?? null;

/** The folder a delegated call writes into: "" is apps/{issuer}; anything else must exist and be visible. */
function destinationFolder(context, parentPath) {
  const appFolder = `apps/${context.issuer}`;
  if (parentPath === '') return appFolder;
  const path = entryPath(parentPath);
  if (!path) throw refuse(404, 'not_found');
  const inside = path === appFolder || path.startsWith(`${appFolder}/`);
  if (!context.firstParty && !inside) throw refuse(404, 'not_found');
  if (path === appFolder || path === 'apps') return path;
  const folder = findEntry(context.owner, path);
  if (!folder || folder.entry_type !== 'folder') throw refuse(404, 'not_found');
  return path;
}

function encodeSegment(segment) {
  return encodeURIComponent(segment).replace(/%3A/gi, ':');
}

function permanentUrl(entry) {
  const owner = state.accounts[entry.owner]?.id ?? entry.owner;
  return `${SITE}/${encodeSegment(owner)}/${entry.path.split('/').map(encodeSegment).join('/')}`;
}

function effectiveLink(entry) {
  const now = Date.now();
  const live = (candidate) => candidate?.link?.enabled && (!candidate.link.expires_at || Date.parse(candidate.link.expires_at) > now);
  const own = live(entry);
  let inheritedFrom = null;
  const segments = entry.path.split('/');
  for (let depth = segments.length - 1; depth > 0 && !inheritedFrom; depth--) {
    const ancestor = findEntry(entry.owner, segments.slice(0, depth).join('/'));
    if (ancestor && live(ancestor)) inheritedFrom = ancestor.id;
  }
  return { own: Boolean(own), inheritedFrom, effective: Boolean(own || inheritedFrom), expiresAt: own && entry.link.expires_at ? entry.link.expires_at : null };
}

function linkAccess(entry) {
  const link = effectiveLink(entry);
  return { url: link.effective ? permanentUrl(entry) : null, enabled: link.own, effective: link.effective, inherited_from: link.inheritedFrom, can_manage: true, expires_at: link.expiresAt };
}

function entryView(entry) {
  return {
    id: entry.id, entry_type: entry.entry_type, name: entry.name, path: entry.path, parent_id: entry.parent_id ?? null, permanent_url: permanentUrl(entry),
    content_type: entry.content_type ?? null, size: entry.size ?? null, origin_app_id: entry.origin_app_id, created_at: entry.created_at, updated_at: entry.updated_at,
  };
}

/** Repeating a mutation with the same operation_id and the same body answers the same; another body is a conflict. */
function replay(context, route, operationId, body) {
  const key = `${route}|${context.owner}|${context.issuer}|${operationId.toLowerCase()}`;
  const fingerprint = createHash('sha256').update(JSON.stringify(body)).digest('hex');
  const kept = state.mutations[key];
  if (kept && kept.fingerprint !== fingerprint) throw refuse(409, 'idempotency_key_reused');
  return { key, fingerprint, kept };
}

// ------------------------------------------------------------------------------------------------ uploads

const operationKey = (context, operationId) => `${context.owner}|${context.issuer}|${operationId.toLowerCase()}`;

function expire(upload) {
  if (['reserved', 'receiving', 'staged'].includes(upload.state) && Date.parse(upload.expires_at) <= Date.now()) {
    upload.state = 'expired';
    upload.capability_hash = null;
    rmSync(join(DIR, 'staging', upload.upload_id), { force: true });
  }
}

const statusOf = (upload) => ({ operation_id: upload.operation_id, upload_id: upload.upload_id, state: upload.state, expires_at: upload.expires_at, published_entry_id: upload.published_entry_id ?? null });
const hashCapability = (capability) => createHash('sha256').update(capability).digest('hex');

async function reserve(req) {
  const context = await authorizeProof(req, 'briefcase.uploads.reserve');
  const body = shape(await readJsonBody(req), { operation_id: 'uuid', parent_path: 'string', name: 'string', content_type: 'string', size: 'u64', sha256: 'string' });
  requireOperationId(body.operation_id);
  if (body.content_type.length > 255 || !/^[a-z0-9!#$&^_.+-]+\/[a-z0-9!#$&^_.+-]+(\s*;.*)?$/i.test(body.content_type)) throw refuse(422, 'invalid_content_type');
  if (!/^[0-9a-f]{64}$/.test(body.sha256)) throw refuse(422, 'invalid_sha256');
  const name = entryName(body.name);
  const parent = destinationFolder(context, body.parent_path);
  const requestHash = createHash('sha256').update(JSON.stringify(body)).digest('hex');
  const existing = state.uploads[state.operations[operationKey(context, body.operation_id)]];
  if (existing) {
    if (existing.request_hash !== requestHash) throw refuse(409, 'idempotency_key_reused');
    expire(existing);
    let capability = null;
    if (existing.state === 'reserved') {
      // An unchanged retry may rotate the capability of an idle reservation.
      capability = `bcu_${randomBytes(32).toString('base64url')}`;
      existing.capability_hash = hashCapability(capability);
    }
    save();
    return { status: 200, body: { ...statusOf(existing), capability }, headers: { 'cache-control': 'no-store' } };
  }
  if (body.size > MAX_UPLOAD_BYTES) throw refuse(413, 'payload_too_large', 'The request body exceeds the allowed size.');
  const capability = `bcu_${randomBytes(32).toString('base64url')}`;
  const upload = {
    upload_id: randomUUID(), operation_id: body.operation_id, owner: context.owner, issuer: context.issuer, request_hash: requestHash, parent_path: parent, name,
    content_type: body.content_type, size: body.size, sha256: body.sha256, state: 'reserved', created_at: new Date().toISOString(),
    expires_at: new Date(Date.now() + RESERVATION_TTL_MS).toISOString(), capability_hash: hashCapability(capability), published_entry_id: null,
  };
  state.uploads[upload.upload_id] = upload;
  state.operations[operationKey(context, body.operation_id)] = upload.upload_id;
  save();
  return { status: 200, body: { ...statusOf(upload), capability }, headers: { 'cache-control': 'no-store' } };
}

function single(req, name) {
  const index = req.rawHeaders.findIndex((header, i) => i % 2 === 0 && header.toLowerCase() === name);
  if (index < 0) throw refuse(400, 'missing_upload_header');
  const again = req.rawHeaders.findIndex((header, i) => i % 2 === 0 && i > index && header.toLowerCase() === name);
  if (again >= 0) throw refuse(400, 'duplicate_security_header');
  return req.rawHeaders[index + 1];
}

async function transfer(req, uploadId) {
  // The capability alone authorizes the bytes: account credentials cannot broaden or replace it.
  if (req.headers.authorization) throw refuse(400, 'ambiguous_authentication');
  if (req.headers['transfer-encoding'] || req.headers['content-encoding']) throw refuse(400, 'invalid_upload_framing');
  const capability = single(req, 'x-briefcase-upload-capability');
  const lengthHeader = single(req, 'content-length');
  if (!/^\d+$/.test(lengthHeader)) throw refuse(400, 'invalid_content_length');
  const length = Number(lengthHeader);
  const upload = state.uploads[uploadId];
  const expected = upload?.capability_hash ? Buffer.from(upload.capability_hash, 'hex') : null;
  const given = Buffer.from(hashCapability(capability), 'hex');
  if (!upload || !expected || !timingSafeEqual(expected, given)) throw refuse(404, 'not_found');
  expire(upload);
  if (upload.state !== 'reserved') throw refuse(409, 'delegated_upload_not_reserved');
  if (length !== upload.size) throw refuse(422, 'delegated_upload_manifest_mismatch');
  upload.state = 'receiving';
  save();
  const staging = join(DIR, 'staging', upload.upload_id);
  const hash = createHash('sha256');
  let received = 0;
  try {
    await pipeline(req, new Transform({
      transform(chunk, _encoding, done) {
        received += chunk.length;
        if (received > upload.size) return done(refuse(413, 'payload_too_large', 'Content exceeds the reservation.'));
        hash.update(chunk);
        return done(null, chunk);
      },
    }), createWriteStream(staging, { mode: 0o600 }));
  } catch (error) {
    upload.state = 'reserved';
    upload.capability_hash = null; // reconcile through status and a fresh reserve before retrying
    rmSync(staging, { force: true });
    save();
    if (error instanceof Refusal) throw error;
    throw refuse(400, 'upload_interrupted', 'The bytes did not arrive completely; check the status and reserve again for a fresh capability.');
  }
  if (received !== upload.size || hash.digest('hex') !== upload.sha256) {
    upload.state = 'reserved';
    upload.capability_hash = null;
    rmSync(staging, { force: true });
    save();
    throw refuse(422, 'delegated_upload_manifest_mismatch');
  }
  upload.state = 'staged';
  upload.capability_hash = null;
  save();
  return { status: 200, body: statusOf(upload) };
}

function uploadFor(context, operationId) {
  const upload = state.uploads[state.operations[operationKey(context, operationId)]];
  if (!upload) throw refuse(404, 'not_found');
  expire(upload);
  return upload;
}

async function commit(req) {
  const context = await authorizeProof(req, 'briefcase.uploads.commit');
  const body = shape(await readJsonBody(req), { operation_id: 'uuid', upload_id: 'uuid' });
  requireOperationId(body.operation_id);
  if (body.upload_id === NIL) throw refuse(422, 'invalid_upload_id');
  const upload = uploadFor(context, body.operation_id);
  if (upload.upload_id !== body.upload_id.toLowerCase()) throw refuse(404, 'not_found');
  if (upload.state === 'committed') return { status: 200, body: statusOf(upload) };
  if (upload.state !== 'staged') throw refuse(409, 'delegated_upload_not_staged');
  const blob = join(DIR, 'blobs', upload.sha256);
  const staging = join(DIR, 'staging', upload.upload_id);
  if (existsSync(blob)) rmSync(staging, { force: true });
  else renameSync(staging, blob);
  const path = `${upload.parent_path}/${upload.name}`;
  const now = new Date().toISOString();
  let entry = findEntry(context.owner, path);
  const version = { sha256: upload.sha256, size: upload.size, content_type: upload.content_type, created_at: now, upload_id: upload.upload_id };
  if (entry) {
    if (entry.entry_type !== 'file') throw refuse(409, 'name_conflict', 'A folder of that name exists where the file would be published.');
    Object.assign(entry, { sha256: upload.sha256, size: upload.size, content_type: upload.content_type, updated_at: now });
    entry.versions.push(version);
  } else {
    entry = {
      id: randomUUID(), owner: context.owner, entry_type: 'file', name: upload.name, path, parent_path: upload.parent_path, parent_id: findEntry(context.owner, upload.parent_path)?.id ?? null,
      sha256: upload.sha256, size: upload.size, content_type: upload.content_type, origin_app_id: context.issuer, created_at: now, updated_at: now, versions: [version], link: { enabled: false, expires_at: null },
    };
    state.entries[entry.id] = entry;
  }
  upload.state = 'committed';
  upload.published_entry_id = entry.id;
  save();
  return { status: 200, body: statusOf(upload) };
}

async function status(req) {
  const context = await authorizeProof(req, 'briefcase.uploads.status');
  const body = shape(await readJsonBody(req), { operation_id: 'uuid' });
  requireOperationId(body.operation_id);
  const upload = uploadFor(context, body.operation_id);
  save();
  return { status: 200, body: statusOf(upload) };
}

async function cancel(req) {
  const context = await authorizeProof(req, 'briefcase.uploads.cancel');
  const body = shape(await readJsonBody(req), { operation_id: 'uuid' });
  requireOperationId(body.operation_id);
  const upload = uploadFor(context, body.operation_id);
  // Cancelling never deletes a committed file.
  if (['reserved', 'receiving', 'staged'].includes(upload.state)) {
    upload.state = 'cancelled';
    upload.capability_hash = null;
    rmSync(join(DIR, 'staging', upload.upload_id), { force: true });
  }
  save();
  return { status: 200, body: statusOf(upload) };
}

async function createFolder(req) {
  const context = await authorizeProof(req, 'briefcase.folders.create');
  const body = shape(await readJsonBody(req), { operation_id: 'uuid', parent_path: 'string', name: 'string' });
  requireOperationId(body.operation_id);
  const name = entryName(body.name);
  const parent = destinationFolder(context, body.parent_path);
  const mutation = replay(context, 'folders.create', body.operation_id, body);
  if (mutation.kept) return { status: 201, body: entryView(state.entries[mutation.kept.entry_id]) };
  const path = `${parent}/${name}`;
  if (findEntry(context.owner, path)) throw refuse(409, 'name_conflict', 'An entry of that name already exists in the folder.');
  const now = new Date().toISOString();
  const entry = { id: randomUUID(), owner: context.owner, entry_type: 'folder', name, path, parent_path: parent, parent_id: findEntry(context.owner, parent)?.id ?? null, origin_app_id: context.issuer, created_at: now, updated_at: now, versions: [], link: { enabled: false, expires_at: null } };
  state.entries[entry.id] = entry;
  state.mutations[mutation.key] = { fingerprint: mutation.fingerprint, entry_id: entry.id };
  save();
  return { status: 201, body: entryView(entry) };
}

async function setLinkAccess(req) {
  const context = await authorizeProof(req, 'briefcase.link_access.update');
  const body = shape(await readJsonBody(req), { operation_id: 'uuid', entry_id: 'uuid', enabled: 'boolean', expires_in_minutes: 'number?' });
  requireOperationId(body.operation_id);
  if (body.expires_in_minutes !== undefined && body.expires_in_minutes !== null && (!Number.isInteger(body.expires_in_minutes) || body.expires_in_minutes < 1 || body.expires_in_minutes > 43200)) throw refuse(400, 'invalid_json');
  const entry = state.entries[body.entry_id.toLowerCase()];
  const appFolder = `apps/${context.issuer}`;
  if (!entry || entry.owner !== context.owner || entry.deleted_at || (!context.firstParty && !entry.path.startsWith(`${appFolder}/`))) throw refuse(404, 'not_found');
  const mutation = replay(context, 'link-access', body.operation_id, body);
  if (mutation.kept) return { status: 200, body: linkAccess(entry) };
  if (body.enabled) {
    const permanent = entry.link?.enabled && !entry.link.expires_at;
    if (permanent && body.expires_in_minutes) throw refuse(409, 'link_already_permanent', 'This link is already permanent; a permanent link is never shortened.');
    entry.link = { enabled: true, expires_at: body.expires_in_minutes ? new Date(Date.now() + body.expires_in_minutes * 60_000).toISOString() : null };
  } else {
    entry.link = { enabled: false, expires_at: null };
  }
  entry.updated_at = new Date().toISOString();
  state.mutations[mutation.key] = { fingerprint: mutation.fingerprint };
  save();
  return { status: 200, body: linkAccess(entry) };
}

// ------------------------------------------------------------------------------------------------ public reads

function ownerUuid(segment) {
  const owner = decodeURIComponent(segment);
  if (state.accounts[owner]) return owner;
  const found = Object.values(state.accounts).find((account) => account.id === owner);
  return found?.uuid ?? null;
}

function parseRange(header, size) {
  if (!header) return null;
  const match = /^bytes=(\d*)-(\d*)$/.exec(String(header).trim());
  if (!match || (match[1] === '' && match[2] === '')) return null; // malformed or multi-range: the whole file
  let start;
  let end;
  if (match[1] === '') {
    const suffix = Number(match[2]);
    if (suffix === 0) return 'unsatisfiable';
    start = Math.max(0, size - suffix);
    end = size - 1;
  } else {
    start = Number(match[1]);
    end = match[2] === '' ? size - 1 : Math.min(Number(match[2]), size - 1);
  }
  if (start >= size || start > end) return 'unsatisfiable';
  return { start, end };
}

function disposition(kind, name) {
  const ascii = [...name].map((c) => (c >= ' ' && c <= '~' && c !== '"' && c !== '\\' ? c : '_')).join('');
  return `${kind}; filename="${ascii}"; filename*=UTF-8''${encodeURIComponent(name)}`;
}

function publicRead(req, res, ownerSegment, pathSegments, url) {
  const owner = ownerUuid(ownerSegment);
  const path = entryPath(pathSegments.map((segment) => decodeURIComponent(segment)).join('/'));
  const entry = owner && path ? findEntry(owner, path) : null;
  if (!entry || !effectiveLink(entry).effective) throw refuse(404, 'not_found');
  const view = url.searchParams.get('view') ?? 'metadata';
  const headers = { 'cache-control': 'no-store', 'x-dm-stand-in': 'briefcase' };
  const publicEntry = { id: entry.id, name: entry.name, path: entry.path, entry_type: entry.entry_type, content_type: entry.content_type ?? null, size: entry.size ?? null };
  if (view === 'metadata') return sendJson(res, 200, publicEntry, headers);
  if (view === 'contents') {
    if (entry.entry_type !== 'folder') throw refuse(404, 'not_found');
    const items = Object.values(state.entries).filter((child) => child.owner === owner && child.parent_path === entry.path && !child.deleted_at)
      .map((child) => ({ id: child.id, name: child.name, path: child.path, entry_type: child.entry_type, content_type: child.content_type ?? null, size: child.size ?? null }));
    return sendJson(res, 200, { items, next_cursor: null }, headers);
  }
  if (view !== 'inline' && view !== 'attachment') throw refuse(400, 'invalid_view');
  if (entry.entry_type === 'folder') {
    if (view === 'attachment') throw refuse(422, 'archive_not_served', 'The stand-in does not build folder archives; download the files one by one.');
    throw refuse(404, 'not_found');
  }
  const blob = join(DIR, 'blobs', entry.sha256);
  const size = statSync(blob).size;
  const range = parseRange(req.headers.range, size);
  if (range === 'unsatisfiable') {
    return sendJson(res, 416, { error: { code: 'range_not_satisfiable', message: 'The requested byte range is outside the content.' } }, { 'content-range': `bytes */${size}` });
  }
  const download = view === 'attachment';
  const outgoing = {
    ...headers,
    'content-type': download ? 'application/octet-stream' : entry.content_type,
    'content-length': range ? range.end - range.start + 1 : size,
    'accept-ranges': 'bytes',
    'content-disposition': disposition(download ? 'attachment' : 'inline', entry.name),
    'content-security-policy': download ? "sandbox allow-downloads; default-src 'none'; frame-ancestors 'none'" : "sandbox; default-src 'none'; frame-ancestors 'none'",
    'x-content-type-options': 'nosniff',
    'referrer-policy': 'no-referrer',
    'cross-origin-resource-policy': 'same-origin',
    etag: `"${entry.sha256.slice(0, 32)}"`,
    ...(range ? { 'content-range': `bytes ${range.start}-${range.end}/${size}` } : {}),
  };
  res.writeHead(range ? 206 : 200, outgoing);
  if (req.method === 'HEAD') return res.end();
  return createReadStream(blob, range ? { start: range.start, end: range.end } : {}).pipe(res);
}

// ------------------------------------------------------------------------------------------------ the server

async function controls(req, res, url) {
  if (url.pathname === '/_health') return sendJson(res, 200, { ok: true, service: 'briefcase-stand-in', port: PORT, accounts_api_url: ACCOUNTS_API_URL, data_dir: DIR });
  if (url.pathname === '/_state') {
    const uploads = Object.values(state.uploads).map(({ capability_hash: _hash, request_hash: _request, ...rest }) => rest);
    return sendJson(res, 200, { accounts: Object.values(state.accounts), entries: Object.values(state.entries).map((entry) => ({ ...entryView(entry), owner: entry.owner, link: linkAccess(entry) })), uploads });
  }
  if (url.pathname === '/_requests') {
    if (req.method === 'DELETE') {
      recent.length = 0;
      return sendJson(res, 200, { cleared: true });
    }
    return sendJson(res, 200, { count: recent.length, items: recent });
  }
  if (url.pathname === '/_faults') {
    if (req.method === 'GET') return sendJson(res, 200, { faults });
    if (req.method === 'DELETE') {
      faults = [];
      return sendJson(res, 200, { faults });
    }
    const fault = await readJsonBody(req).catch(() => null);
    const routes = ['reserve', 'transfer', 'commit', 'status', 'cancel', 'link-access', 'folders-create', 'public'];
    if (!fault || !routes.includes(fault.route)) return sendJson(res, 422, { error: { code: 'invalid_route', message: `route must be one of ${routes.join(', ')}.` } });
    const entry = { route: fault.route, status: fault.status ?? 503, code: fault.code ?? 'dependency_unavailable', message: fault.message ?? 'A fault set with POST /_faults.', remaining: Math.max(1, Number(fault.count ?? 1)) };
    faults.push(entry);
    return sendJson(res, 201, { fault: entry, faults });
  }
  return sendJson(res, 404, { error: { code: 'not_found', message: `No stand-in control at ${url.pathname}.` } });
}

const ROUTES = {
  'POST /api/v1/obo/uploads/reserve': ['reserve', reserve],
  'POST /api/v1/obo/uploads/commit': ['commit', commit],
  'POST /api/v1/obo/uploads/status': ['status', status],
  'POST /api/v1/obo/uploads/cancel': ['cancel', cancel],
  'POST /api/v1/obo/folders/create': ['folders-create', createFolder],
  'POST /api/v1/obo/link-access': ['link-access', setLinkAccess],
};
const OWNER = /^(c:|si:)[a-z0-9_-]+$|^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

const server = http.createServer(async (req, res) => {
  const requestId = randomUUID();
  const started = Date.now();
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const log = (statusCode, extra = {}) => record({ method: req.method, path: url.pathname.replace(/\/content$/, '/{upload_id}/content').replace(/uploads\/[0-9a-f-]{36}\//, 'uploads/'), status: statusCode, ms: Date.now() - started, request_id: requestId, ...extra });
  try {
    if (url.pathname.startsWith('/_')) return await controls(req, res, url);
    const key = `${req.method} ${url.pathname}`;
    const transferMatch = /^\/api\/v1\/obo\/uploads\/([0-9a-fA-F-]{36})\/content$/.exec(url.pathname);
    const routeName = ROUTES[key]?.[0] ?? (transferMatch ? 'transfer' : url.pathname.startsWith('/api/v1/public/') ? 'public' : null);
    const fault = routeName ? takeFault(routeName) : null;
    if (fault) {
      req.resume();
      sendJson(res, fault.status, { error: { code: fault.code, message: fault.message, request_id: requestId } });
      return log(fault.status, { fault: true });
    }
    if (ROUTES[key]) {
      const answer = await ROUTES[key][1](req);
      sendJson(res, answer.status, answer.body, answer.headers);
      return log(answer.status, { state: answer.body.state });
    }
    if (transferMatch && req.method === 'PUT') {
      const answer = await transfer(req, transferMatch[1].toLowerCase());
      sendJson(res, answer.status, answer.body);
      return log(answer.status, { bytes_in: Number(req.headers['content-length'] || 0), state: answer.body.state });
    }
    if (url.pathname === '/api/v1/obo/files' && req.method === 'POST') {
      req.resume();
      throw refuse(410, 'obo_upload_retired', 'Raw OBO uploads are retired. Use delegated upload reserve, transfer, and commit.');
    }
    const segments = url.pathname.split('/').filter(Boolean);
    if (segments[0] === 'api' && segments[1] === 'v1' && segments[2] === 'public' && segments.length >= 5 && (req.method === 'GET' || req.method === 'HEAD')) {
      publicRead(req, res, segments[3], segments.slice(4), url);
      return log(res.statusCode, { view: url.searchParams.get('view') ?? 'metadata' });
    }
    if (segments.length >= 2 && OWNER.test(decodeURIComponent(segments[0])) && (req.method === 'GET' || req.method === 'HEAD')) {
      // A permanent file address: the website shows it; the stand-in sends the browser to the inline bytes.
      res.writeHead(302, { location: `/api/v1/public/${segments.join('/')}?view=inline`, 'x-dm-stand-in': 'briefcase' });
      res.end();
      return log(302);
    }
    req.resume();
    throw refuse(404, 'not_found');
  } catch (error) {
    const refusal = error instanceof Refusal ? error : refuse(500, 'internal_error', `The stand-in failed: ${error.message}`);
    if (!res.headersSent) sendError(res, refusal, requestId);
    else res.destroy();
    save();
    return log(refusal.status, { code: refusal.code });
  }
});

server.requestTimeout = 0;
server.listen(PORT, '127.0.0.1', () => {
  process.stdout.write(`briefcase stand-in listening on http://127.0.0.1:${PORT} (data ${DIR}, proofs verified at ${ACCOUNTS_API_URL})\n`);
});
for (const signal of ['SIGTERM', 'SIGINT']) {
  process.on(signal, () => {
    server.close();
    setTimeout(() => process.exit(0), 300).unref();
  });
}
