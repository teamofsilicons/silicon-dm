/**
 * Uploads through Silicon Briefcase, done by this website's server (spec §6.9). DM's service never handles files: the
 * website saves a Carbon's file or voice note into the Carbon's own Briefcase, in `apps/dm/`, as DM acting for the
 * Carbon with User verification proofs from Silicon Accounts, and the message carries only the public link.
 *
 *   BRIEFCASE_API_URL   Briefcase's API origin (https://api.briefcase.teamofsilicons.com). Unset: uploads are off, and
 *                       GET /api/uploads/config says so (links and GIFs still work). Plain http only for a stand-in on
 *                       this machine.
 *   UPLOADS_MAX_BYTES   the largest file this website takes, in bytes [209715200, 200 MiB]
 *
 * One upload (POST /api/uploads), in this order:
 *   1. Accounts POST /v1/proofs/user-verification (Basic APP_ID:APP_SECRET, subject_token = the session's access token,
 *      receiving_app briefcase, the five upload scopes, 600 s tokens, an Idempotency-Key). It comes first, while the
 *      access token is surely valid: the proof then lives on its own refresh token, however long the bytes take.
 *   2. the bytes into a temp file, with their SHA-256 and the size cap (Briefcase wants both before the transfer)
 *   3. Briefcase POST /api/v1/obo/uploads/reserve {operation_id, parent_path "", name, content_type, size, sha256}
 *   4. PUT /api/v1/obo/uploads/{upload_id}/content with X-Briefcase-Upload-Capability, the exact Content-Length (a
 *      file-backed Blob, never chunked) and no Authorization
 *   5. POST /api/v1/obo/uploads/commit {operation_id, upload_id}
 *   6. POST /api/v1/obo/link-access {operation_id, entry_id, enabled: true}: anyone with the link can view
 *   7. Accounts POST /v1/proofs/revoke {proof_id}, and the temp file is removed, whatever happened.
 * Every Briefcase request carries a proof token never sent before (Briefcase's rule: a spent proof is never retried); a
 * request that failed on the way is repeated once with a fresh token and the same operation, which Briefcase answers
 * idempotently. Briefcase keeps one file per path, so each upload gets a name of its own (a time and a random part
 * before the file's name).
 *
 * The link: Briefcase's public inline URL, /api/v1/public/{owner uuid}/apps/dm/{name}?view=inline. Briefcase answers
 * those bytes with Cross-Origin-Resource-Policy: same-origin, so the browser loads them through GET /api/uploads/file
 * (this module's `publicFileTarget` and `fileResponseHeaders` decide what that proxy may fetch and how it answers).
 *
 * Nothing here logs a file's contents, its name, a token or a proof.
 */
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { createWriteStream, openAsBlob } from "node:fs";
import { rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable, Transform } from "node:stream";
import type { ReadableStream as NodeReadableStream } from "node:stream/web";
import { pipeline } from "node:stream/promises";
import { basicAuth, type ServerEnv } from "./env";
import { errorBody } from "./session";

/* ------------------------------------------------------------------------------------------------------------------ */
/* Settings                                                                                                            */
/* ------------------------------------------------------------------------------------------------------------------ */

export const DEFAULT_UPLOADS_MAX_BYTES = 200 * 1024 * 1024;
const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"]);

export type UploadsSettings =
  | {
      enabled: true;
      maxBytes: number;
      /** Where this server calls Briefcase (no trailing slash): the origin, and a path prefix if it has one. */
      apiBase: string;
      /** What links in messages start with. The same as apiBase, except a plain-http stand-in on this machine: DM's
       * service accepts only https links, so its links are written with https and the proxy maps them back. */
      publicBase: string;
    }
  | { enabled: false; maxBytes: number; reason: string };

const NOT_CONNECTED = "Uploads are off: this DM website is not connected to Silicon Briefcase (BRIEFCASE_API_URL is not set). Links and GIFs still work.";

/** Reads BRIEFCASE_API_URL and UPLOADS_MAX_BYTES (at request time, like every setting of this site). */
export function uploadsSettings(source: Record<string, string | undefined> = process.env): UploadsSettings {
  const maxRaw = source.UPLOADS_MAX_BYTES?.trim();
  const maxBytes = maxRaw ? Number(maxRaw) : DEFAULT_UPLOADS_MAX_BYTES;
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) {
    return { enabled: false, maxBytes: 0, reason: `Uploads are off: UPLOADS_MAX_BYTES is "${maxRaw}", which is not a whole number of bytes (209715200 is 200 MB).` };
  }
  const raw = source.BRIEFCASE_API_URL?.trim();
  if (!raw) return { enabled: false, maxBytes, reason: NOT_CONNECTED };
  // The reason reaches every browser (GET /api/uploads/config needs no sign-in), so it never repeats the value itself:
  // a mistaken one could carry credentials.
  const off = (problem: string) => ({ enabled: false as const, maxBytes, reason: `Uploads are off: ${problem}` });
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return off("BRIEFCASE_API_URL is not a URL (it looks like https://api.briefcase.teamofsilicons.com).");
  }
  const loopbackHttp = url.protocol === "http:" && LOOPBACK.has(url.hostname);
  if (url.protocol !== "https:" && !loopbackHttp) return off("BRIEFCASE_API_URL must start with https:// (plain http only for a stand-in on this machine).");
  if (url.username || url.password || url.search || url.hash) return off("BRIEFCASE_API_URL must be an origin (and a path) without credentials, a query or a #fragment.");
  // Accept the API's base either way: "https://api.example" or "https://api.example/api/v1".
  const path = url.pathname.replace(/\/+$/, "").replace(/\/api\/v1$/, "");
  const apiBase = `${url.origin}${path}`;
  return { enabled: true, maxBytes, apiBase, publicBase: loopbackHttp ? `https://${url.host}${path}` : apiBase };
}

export type EnabledUploads = Extract<UploadsSettings, { enabled: true }>;

/* ------------------------------------------------------------------------------------------------------------------ */
/* Failures in the words a Carbon reads                                                                                */
/* ------------------------------------------------------------------------------------------------------------------ */

/** A failed upload, answered as `{"error": {code, message, hint}}` with its status (and Retry-After when it has one). */
export class UploadFailure extends Error {
  readonly status: number;
  readonly code: string;
  readonly hint: string | undefined;
  readonly retryAfter: number | undefined;

  constructor(status: number, code: string, message: string, hint?: string, retryAfter?: number) {
    super(message);
    this.name = "UploadFailure";
    this.status = status;
    this.code = code;
    this.hint = hint;
    this.retryAfter = retryAfter;
  }

  body() {
    return errorBody(this.code, this.message, this.hint);
  }

  /** Anything thrown during an upload, as the failure the browser hears. */
  static from(error: unknown): UploadFailure {
    if (error instanceof UploadFailure) return error;
    return new UploadFailure(500, "upload_failed", "Something on this website failed while saving the file, so it was not sent.", "Try again; if it keeps failing, report it in Settings → Report a problem.");
  }
}

export const formatBytes = (bytes: number) => {
  if (bytes >= 1024 * 1024 * 1024) return `${Number((bytes / 1024 ** 3).toFixed(1))} GB`;
  if (bytes >= 1024 * 1024) return `${Number((bytes / 1024 ** 2).toFixed(1))} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} bytes`;
};

export const tooLarge = (maxBytes: number) =>
  new UploadFailure(413, "file_too_large", `The file is larger than ${formatBytes(maxBytes)}, the most this DM website takes.`, "Make it smaller, or share a link to it instead (+ → Add a link).");

/**
 * Uploads one account may have on this server at once. A tab sends three at a time (the composer's tray), so this
 * leaves room for two tabs, and keeps one browser from filling this server's temporary disk with files in parallel.
 */
export const MAX_UPLOADS_PER_ACCOUNT = 6;
const uploadsInFlight = new Map<string, number>();

/** Takes one of the account's upload slots: a function that gives it back (call it once), or null when all are taken. */
export function claimUploadSlot(account: string): (() => void) | null {
  const running = uploadsInFlight.get(account) ?? 0;
  if (running >= MAX_UPLOADS_PER_ACCOUNT) return null;
  uploadsInFlight.set(account, running + 1);
  let given = false;
  return () => {
    if (given) return;
    given = true;
    const left = (uploadsInFlight.get(account) ?? 1) - 1;
    if (left > 0) uploadsInFlight.set(account, left);
    else uploadsInFlight.delete(account);
  };
}

export const tooManyUploads = () =>
  new UploadFailure(429, "too_many_uploads", `You already have ${MAX_UPLOADS_PER_ACCOUNT} files uploading here, so this one did not start.`, "Retry it when one of them is done.", 5);

const unavailable = (who: "Silicon Briefcase" | "Silicon Accounts", retryAfter?: number) =>
  new UploadFailure(503, who === "Silicon Briefcase" ? "briefcase_unavailable" : "accounts_unavailable", `${who} did not answer, so the file was not saved.`, "Try again in a moment.", retryAfter);

/* ------------------------------------------------------------------------------------------------------------------ */
/* The file: name, type, bytes                                                                                         */
/* ------------------------------------------------------------------------------------------------------------------ */

const BRIEFCASE_NAME_MAX = 255;
/** "20261017-153212-k3f9a2-": what storedName puts before the file's own name. */
const PREFIX_LENGTH = 23;
const STORED = /^\d{8}-\d{6}-[0-9a-z]{6}-(.+)$/;

/** Shortens a name to `max` characters, keeping its extension. */
function shorten(name: string, max: number): string {
  const characters = [...name];
  if (characters.length <= max) return name;
  const dot = name.lastIndexOf(".");
  const extension = dot > 0 && name.length - dot <= 16 ? name.slice(dot) : "";
  const stem = [...name.slice(0, name.length - extension.length)].slice(0, max - [...extension].length).join("").trimEnd();
  return `${stem}${extension}`;
}

/**
 * Characters that change how the rest of a name is drawn without being seen: bidirectional overrides and isolates (a
 * right-to-left override turns "invoice‮gnp.exe" into "invoiceexe.png" on screen) and the directional marks.
 */
const INVISIBLE_DIRECTION = /[؜‎‏‪-‮⁦-⁩]/g;

/**
 * The name a person sees: the file's own name with folders, control characters, invisible direction changes and runs
 * of spaces taken out, short enough that Briefcase takes it with storedName's prefix. Null when nothing usable is left.
 */
export function cleanFileName(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const name = raw.normalize("NFC").replace(/[\u0000-\u001f\u007f]/g, "").replace(INVISIBLE_DIRECTION, "").replace(/[\\/]/g, "-").replace(/\s+/g, " ").trim();
  if (!name || name === "." || name === "..") return null;
  return shorten(name, BRIEFCASE_NAME_MAX - PREFIX_LENGTH);
}

/** Briefcase keeps one file per path, so every upload gets a name of its own: when, a random part, then its name. */
export function storedName(name: string, now: Date = new Date(), random: Buffer = randomBytes(6)): string {
  const stamp = now.toISOString().replace(/[-:]/g, "").replace("T", "-").slice(0, 15);
  const tag = [...random].map(byte => "0123456789abcdefghijklmnopqrstuvwxyz"[byte % 36]).join("").slice(0, 6);
  return `${stamp}-${tag}-${name}`;
}

/** The file's own name again, from a stored one (downloads are named after the file, not after its stored name). */
export const originalName = (stored: string) => STORED.exec(stored)?.[1] ?? stored;

const MEDIA_TYPE = /^[a-z0-9!#$&^_.+-]+\/[a-z0-9!#$&^_.+-]+(\s*;.*)?$/i;
const BY_EXTENSION: Record<string, string> = {
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", avif: "image/avif", heic: "image/heic", heif: "image/heif",
  svg: "image/svg+xml", bmp: "image/bmp", pdf: "application/pdf", txt: "text/plain", md: "text/markdown", csv: "text/csv", json: "application/json",
  zip: "application/zip", mp3: "audio/mpeg", m4a: "audio/mp4", wav: "audio/wav", ogg: "audio/ogg", oga: "audio/ogg", opus: "audio/ogg", weba: "audio/webm",
  flac: "audio/flac", aac: "audio/aac", mp4: "video/mp4", m4v: "video/mp4", mov: "video/quicktime", webm: "video/webm",
  doc: "application/msword", docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", xls: "application/vnd.ms-excel",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ppt: "application/vnd.ms-powerpoint",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
};

/** The file's type: what the browser said, or (when it said nothing useful) what its extension says. */
export function contentTypeFor(header: string | null | undefined, name: string): string {
  const declared = header?.trim() ?? "";
  const valid = declared.length > 0 && declared.length <= 255 && MEDIA_TYPE.test(declared);
  if (valid && !/^application\/octet-stream\b/i.test(declared)) return declared;
  const dot = name.lastIndexOf(".");
  const byName = dot > 0 ? BY_EXTENSION[name.slice(dot + 1).toLowerCase()] : undefined;
  return byName ?? "application/octet-stream";
}

export interface ReceivedFile {
  path: string;
  size: number;
  sha256: string;
}

/**
 * Streams the request's body into a temp file (0600) with its SHA-256, stopping at `maxBytes` (413). The file is
 * removed again when anything goes wrong; on success the caller removes it.
 */
export async function receiveFile(body: ReadableStream<Uint8Array> | null, maxBytes: number, signal?: AbortSignal): Promise<ReceivedFile> {
  if (!body) throw new UploadFailure(422, "file_empty", "The upload carried no file.", "Choose a file and try again.");
  const path = join(tmpdir(), `dm-upload-${randomUUID()}`);
  const hash = createHash("sha256");
  let size = 0;
  const meter = new Transform({
    transform(chunk: Buffer, _encoding, done) {
      size += chunk.length;
      if (size > maxBytes) return done(tooLarge(maxBytes));
      hash.update(chunk);
      done(null, chunk);
    },
  });
  try {
    await pipeline(Readable.fromWeb(body as unknown as NodeReadableStream<Uint8Array>), meter, createWriteStream(path, { mode: 0o600 }), signal ? { signal } : {});
  } catch (error) {
    await rm(path, { force: true });
    if (error instanceof UploadFailure) throw error;
    throw new UploadFailure(400, "upload_interrupted", "The file did not arrive completely, so nothing was saved.", "Check the connection, then try again.");
  }
  if (size === 0) {
    await rm(path, { force: true });
    throw new UploadFailure(422, "file_empty", "This file is empty (0 bytes), so there is nothing to send.", "Choose another file.");
  }
  return { path, size, sha256: hash.digest("hex") };
}

/** Reads and drops what is left of a body (at most `limit` bytes), so a refusal reaches a browser that is still sending. */
export async function drain(body: ReadableStream<Uint8Array> | null, limit: number): Promise<void> {
  if (!body) return;
  const reader = body.getReader();
  let read = 0;
  try {
    while (read <= limit) {
      const { done, value } = await reader.read();
      if (done) return;
      read += value.byteLength;
    }
    await reader.cancel();
  } catch {
    // The browser went away: nothing left to answer.
  }
}

/* ------------------------------------------------------------------------------------------------------------------ */
/* Proofs (Silicon Accounts)                                                                                           */
/* ------------------------------------------------------------------------------------------------------------------ */

/** What DM may do at Briefcase for the Carbon during one upload, and nothing else. */
export const BRIEFCASE_SCOPES = ["briefcase.uploads.reserve", "briefcase.uploads.commit", "briefcase.uploads.status", "briefcase.uploads.cancel", "briefcase.link_access.update"];
const PROOF_TTL_SECONDS = 600;
/** A proof token with less than this left is not sent; the next one is asked for instead. */
const TOKEN_MARGIN_MS = 60_000;
const ACCOUNTS_TIMEOUT_MS = 15_000;

interface ProofAnswer {
  proof_id?: string;
  proof_token?: string;
  proof_refresh_token?: string;
  expires_at?: string;
}

interface Answer {
  status: number;
  body: Record<string, unknown> | null;
  headers: Headers;
}

const errorOf = (answer: Answer) => {
  const error = answer.body?.error as { code?: unknown; message?: unknown } | undefined;
  return { code: typeof error?.code === "string" ? error.code : "", message: typeof error?.message === "string" ? error.message : "" };
};

async function readAnswer(response: Response): Promise<Answer> {
  const text = await response.text().catch(() => "");
  let body: Record<string, unknown> | null = null;
  try {
    body = text ? (JSON.parse(text) as Record<string, unknown>) : null;
  } catch {
    body = null;
  }
  return { status: response.status, body, headers: response.headers };
}

/** One call to the Accounts API with this app's credentials; a network failure is tried once more with the same key. */
async function accountsCall(env: ServerEnv, path: string, payload: unknown, idempotencyKey?: string): Promise<Answer> {
  for (let attempt = 0; ; attempt++) {
    try {
      const response = await fetch(`${env.accountsApiUrl}${path}`, {
        method: "POST",
        headers: { Authorization: basicAuth(env), "Content-Type": "application/json", Accept: "application/json", ...(idempotencyKey ? { "Idempotency-Key": idempotencyKey } : {}) },
        body: JSON.stringify(payload),
        cache: "no-store",
        signal: AbortSignal.timeout(ACCOUNTS_TIMEOUT_MS),
      });
      return await readAnswer(response);
    } catch {
      if (attempt >= 1 || !idempotencyKey) return { status: 0, body: null, headers: new Headers() };
    }
  }
}

/** Why Silicon Accounts would not give DM a proof, as the browser hears it. */
function proofRefused(answer: Answer): UploadFailure {
  if (answer.status === 0 || answer.status >= 500) return unavailable("Silicon Accounts");
  const { code, message } = errorOf(answer);
  if (["invalid_subject_token", "membership_inactive", "account_not_active", "subject_token_wrong_app"].includes(code)) {
    return new UploadFailure(401, "signed_out", "Your sign-in at DM no longer lets it act for you, so the file was not saved.", "Sign in again, then send the file again.");
  }
  if (["unknown_receiving_app", "receiving_app_disabled"].includes(code)) {
    return new UploadFailure(503, "briefcase_unavailable", `Silicon Briefcase is not available to DM right now (Silicon Accounts: ${code}).`, "Send a link instead (+ → Add a link), or try again later.");
  }
  if (["invalid_app_credentials", "app_credentials_required", "app_disabled"].includes(code)) {
    return new UploadFailure(503, "uploads_misconfigured", "This DM website could not prove itself to Silicon Accounts, so uploads are not working.", "Send a link instead; this site's maintainers need to check APP_ID and APP_SECRET.");
  }
  return new UploadFailure(502, "permission_refused", `Silicon Accounts did not let DM act for you${message ? `: ${message}` : ` (HTTP ${answer.status})`}`, "Try again; if it keeps failing, report it in Settings → Report a problem.");
}

/** A proof ended while the file was on its way (its refresh refused). */
function proofEnded(answer: Answer): UploadFailure {
  if (answer.status === 0 || answer.status >= 500) return unavailable("Silicon Accounts");
  const { code } = errorOf(answer);
  return new UploadFailure(409, "permission_ended", `DM's permission to save this file ended before it was saved${code ? ` (${code})` : ""}.`, "Send the file again.");
}

/**
 * A User verification proof for one upload: DM acting for the signed-in Carbon at Briefcase. `fresh()` hands out a
 * proof token that was never sent before (the issued one while it has a minute left, then one new token per call), and
 * keeps only the newest refresh token (a used one presented again would end the proof).
 */
export class UploadProof {
  readonly id: string;
  private unused: { token: string; expiresAt: number } | null;
  private refreshToken: string | null;
  private revoked = false;

  private constructor(private readonly env: ServerEnv, answer: ProofAnswer) {
    this.id = answer.proof_id!;
    this.unused = { token: answer.proof_token!, expiresAt: Date.parse(answer.expires_at ?? "") || Date.now() + PROOF_TTL_SECONDS * 1000 };
    this.refreshToken = answer.proof_refresh_token ?? null;
  }

  static async issue(env: ServerEnv, subjectToken: string): Promise<UploadProof> {
    const answer = await accountsCall(env, "/v1/proofs/user-verification", { subject_token: subjectToken, receiving_app: "briefcase", scopes: BRIEFCASE_SCOPES, access_ttl_seconds: PROOF_TTL_SECONDS }, `dm-upload-${randomUUID()}`);
    const proof = answer.body as ProofAnswer | null;
    if ((answer.status !== 201 && answer.status !== 200) || !proof?.proof_id || !proof.proof_token) throw proofRefused(answer);
    return new UploadProof(env, proof);
  }

  async fresh(): Promise<string> {
    const unused = this.unused;
    this.unused = null;
    if (unused && unused.expiresAt - Date.now() > TOKEN_MARGIN_MS) return unused.token;
    const refresh = this.refreshToken;
    if (!refresh) throw new UploadFailure(409, "permission_ended", "DM's permission to save this file ran out before it was saved.", "Send the file again.");
    // Keyed by the refresh token itself: an answer lost on the way is replayed instead of presenting a used token.
    const key = `refresh-${createHash("sha256").update(refresh).digest("hex").slice(0, 32)}`;
    const answer = await accountsCall(this.env, "/v1/proofs/refresh", { proof_refresh_token: refresh }, key);
    const next = answer.body as ProofAnswer | null;
    if (answer.status !== 200 || !next?.proof_token) throw proofEnded(answer);
    this.refreshToken = next.proof_refresh_token ?? null;
    return next.proof_token;
  }

  /** Ends the proof (every token of it stops verifying). Best effort: a proof left behind still expires. */
  async revoke(): Promise<void> {
    if (this.revoked) return;
    this.revoked = true;
    this.unused = null;
    this.refreshToken = null;
    await accountsCall(this.env, "/v1/proofs/revoke", { proof_id: this.id }).catch(() => undefined);
  }
}

/* ------------------------------------------------------------------------------------------------------------------ */
/* Briefcase                                                                                                           */
/* ------------------------------------------------------------------------------------------------------------------ */

const BRIEFCASE_TIMEOUT_MS = 60_000;
/** The byte transfer may take long (up to the size cap on a slow uplink between servers). */
const TRANSFER_TIMEOUT_MS = 15 * 60_000;

const transient = (status: number) => status === 0 || status === 408 || status === 425 || status === 500 || status === 502 || status === 503 || status === 504;

async function briefcaseFetch(url: string, init: RequestInit, timeout = BRIEFCASE_TIMEOUT_MS): Promise<Answer> {
  try {
    const response = await fetch(url, { ...init, cache: "no-store", redirect: "manual", signal: AbortSignal.timeout(timeout) });
    return await readAnswer(response);
  } catch {
    return { status: 0, body: null, headers: new Headers() };
  }
}

const retryAfterOf = (answer: Answer) => {
  const value = Number(answer.headers.get("retry-after"));
  return Number.isFinite(value) && value > 0 ? value : undefined;
};

/** Why Briefcase did not take the file, as the browser hears it. */
export function briefcaseRefused(answer: Answer, step: string): UploadFailure {
  if (transient(answer.status)) return unavailable("Silicon Briefcase", retryAfterOf(answer));
  const { code, message } = errorOf(answer);
  if (["proof_issuer_not_allowed", "proof_scope_missing", "proof_kind_not_accepted"].includes(code)) {
    return new UploadFailure(503, "briefcase_not_allowed", "Silicon Briefcase does not accept files from DM yet.", "Send a link instead (+ → Add a link). Briefcase's operators allow DM in BRIEFCASE_PROOF_ISSUERS.");
  }
  if (answer.status === 507) return new UploadFailure(507, "briefcase_full", "Your Silicon Briefcase is full, so the file was not saved.", "Free some space in Silicon Briefcase, then try again.");
  if (answer.status === 429) {
    return new UploadFailure(429, "briefcase_limit", "You reached today's upload allowance in Silicon Briefcase.", "It resets at 00:00 UTC; until then, send a link instead.", retryAfterOf(answer));
  }
  if (answer.status === 413) return new UploadFailure(413, "file_too_large", "Silicon Briefcase does not take a file this large.", "Make it smaller, or share a link to it instead (+ → Add a link).");
  if (answer.status === 401) {
    return new UploadFailure(502, "briefcase_refused", `Silicon Briefcase did not accept DM's permission to act for you (${code || "HTTP 401"}).`, "Sign in again, then send the file again.");
  }
  if (answer.status === 422) return new UploadFailure(422, "file_refused", `Silicon Briefcase refused this file${message ? `: ${message}` : ""} (${code || "HTTP 422"}).`, "Rename it or choose another file, then try again.");
  return new UploadFailure(502, "briefcase_refused", `Silicon Briefcase refused the upload at the ${step} step${code ? ` (${code})` : ` (HTTP ${answer.status})`}.`, "Try again; if it keeps failing, report it in Settings → Report a problem.");
}

interface UploadState {
  operation_id?: string;
  upload_id?: string;
  state?: string;
  published_entry_id?: string | null;
  capability?: string | null;
}

/** A call to a delegated route with a fresh proof token; one more attempt (a new token, the same body) when it failed on the way. */
async function delegated(settings: EnabledUploads, proof: UploadProof, route: string, payload: unknown, step: string): Promise<Answer> {
  let answer: Answer = { status: 0, body: null, headers: new Headers() };
  for (let attempt = 0; attempt < 2; attempt++) {
    const token = await proof.fresh();
    answer = await briefcaseFetch(`${settings.apiBase}/api/v1/obo/${route}`, {
      method: "POST",
      headers: { Authorization: `Proof ${token}`, "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(payload),
    });
    if (answer.status === 200 || answer.status === 201) return answer;
    if (!transient(answer.status)) break;
  }
  throw briefcaseRefused(answer, step);
}

/** The bytes, with the reservation's capability alone: no Authorization, the exact Content-Length (a file-backed Blob). */
async function transfer(settings: EnabledUploads, file: ReceivedFile, uploadId: string, capability: string): Promise<Answer> {
  const blob = await openAsBlob(file.path, { type: "application/octet-stream" });
  return briefcaseFetch(`${settings.apiBase}/api/v1/obo/uploads/${encodeURIComponent(uploadId)}/content`, {
    method: "PUT",
    headers: { "X-Briefcase-Upload-Capability": capability, "Content-Type": "application/octet-stream", Accept: "application/json" },
    body: blob,
  }, TRANSFER_TIMEOUT_MS);
}

const encodeSegment = (segment: string) => encodeURIComponent(segment).replace(/%3A/gi, ":");

/** A file's public inline link: /api/v1/public/{owner}/{path}?view=inline under the public base. */
export function publicFileUrl(settings: EnabledUploads, owner: string, path: string[]): string {
  return `${settings.publicBase}/api/v1/public/${encodeSegment(owner)}/${path.map(encodeSegment).join("/")}?view=inline`;
}

/** The entry's path from Briefcase's shareable website URL ({site}/{owner}/{path}), when it names the file we saved. */
function pathFromLink(link: unknown, name: string): string[] | null {
  if (typeof link !== "string") return null;
  try {
    const segments = new URL(link).pathname.split("/").filter(Boolean).map(segment => decodeURIComponent(segment));
    const path = segments.slice(1);
    return path.length >= 2 && path[path.length - 1] === name && !path.some(segment => segment === "." || segment === "..") ? path : null;
  } catch {
    return null;
  }
}

export interface PublishInput {
  settings: EnabledUploads;
  proof: UploadProof;
  file: ReceivedFile;
  /** The stored name (storedName). */
  name: string;
  contentType: string;
  /** The Carbon's account uuid: links name the owner by uuid, which never changes (an id can). */
  owner: string;
  appId: string;
}

/**
 * Reserve → transfer → commit → link-access. A reservation that cannot be finished is cancelled (best effort); a
 * transfer that failed on the way is reconciled through its status and tried once more with a new capability.
 */
export async function publishToBriefcase(input: PublishInput): Promise<{ url: string }> {
  const { settings, proof, file, name, contentType, owner, appId } = input;
  const operation = randomUUID();
  const manifest = { operation_id: operation, parent_path: "", name, content_type: contentType, size: file.size, sha256: file.sha256 };
  let committed = false;
  let reserved = false;
  try {
    let upload = (await delegated(settings, proof, "uploads/reserve", manifest, "reserve")).body as UploadState;
    reserved = true;
    for (let attempt = 0; upload.state === "reserved" || upload.state === "receiving"; attempt++) {
      if (!upload.upload_id) throw briefcaseRefused({ status: 502, body: null, headers: new Headers() }, "reserve");
      const sent = upload.capability ? await transfer(settings, file, upload.upload_id, upload.capability) : null;
      if (sent?.status === 200) {
        upload = { ...upload, ...(sent.body as UploadState) };
        continue;
      }
      if (sent && !transient(sent.status) && sent.status !== 409) throw briefcaseRefused(sent, "transfer");
      if (attempt >= 1) throw briefcaseRefused(sent ?? { status: 503, body: null, headers: new Headers() }, "transfer");
      // Uncertain: ask where it stands, and reserve again (the same manifest) for a new capability if it still waits.
      upload = (await delegated(settings, proof, "uploads/status", { operation_id: operation }, "status")).body as UploadState;
      if (upload.state === "reserved") upload = (await delegated(settings, proof, "uploads/reserve", manifest, "reserve")).body as UploadState;
    }
    if (upload.state === "staged") {
      upload = (await delegated(settings, proof, "uploads/commit", { operation_id: operation, upload_id: upload.upload_id }, "commit")).body as UploadState;
    }
    if (upload.state !== "committed" || !upload.published_entry_id) {
      throw new UploadFailure(502, "briefcase_refused", `Silicon Briefcase did not publish the file (it is ${upload.state ?? "in an unknown state"}).`, "Try again.");
    }
    committed = true;
    const link = (await delegated(settings, proof, "link-access", { operation_id: randomUUID(), entry_id: upload.published_entry_id, enabled: true }, "link")).body as { url?: unknown; effective?: unknown };
    if (link.effective !== true) {
      throw new UploadFailure(502, "briefcase_refused", "Silicon Briefcase saved the file but did not open a link to it.", "Try again; the file stays in your Briefcase under apps/dm.");
    }
    return { url: publicFileUrl(settings, owner, pathFromLink(link.url, name) ?? ["apps", appId, name]) };
  } catch (error) {
    if (reserved && !committed) {
      // An abandoned reservation is cancelled with a proof of its own; cancelling never touches a published file.
      await delegated(settings, proof, "uploads/cancel", { operation_id: operation }, "cancel").catch(() => undefined);
    }
    throw error;
  }
}

/* ------------------------------------------------------------------------------------------------------------------ */
/* The file proxy (GET /api/uploads/file)                                                                              */
/* ------------------------------------------------------------------------------------------------------------------ */

export type FileTarget = { upstream: string; download: boolean; fileName: string } | { problem: string };

const PUBLIC_PATH = "/api/v1/public/";

/**
 * What the proxy may fetch for a link from a message: only Briefcase's public files, from the configured Briefcase,
 * by their own path (never another host, never a path that could climb out). The upstream is always the inline view;
 * the proxy decides how the browser treats the bytes.
 */
export function publicFileTarget(settings: EnabledUploads, raw: string | null): FileTarget {
  if (!raw) return { problem: "Say which file: /api/uploads/file?url=<a Silicon Briefcase public link>." };
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { problem: "The url parameter is not a link." };
  }
  if (url.username || url.password) return { problem: "Links with credentials are never fetched." };
  let rest: string | null = null;
  for (const base of new Set([settings.publicBase, settings.apiBase])) {
    const prefix = new URL(base);
    const start = `${prefix.pathname.replace(/\/+$/, "")}${PUBLIC_PATH}`;
    if (url.origin === prefix.origin && url.pathname.startsWith(start)) rest = url.pathname.slice(start.length);
  }
  if (rest === null) return { problem: `Only Silicon Briefcase's public files are shown through DM (${settings.publicBase}${PUBLIC_PATH}…).` };
  const segments = rest.split("/");
  if (segments.length < 2) return { problem: "A Briefcase public link names its owner and the file's path." };
  for (const segment of segments) {
    if (!segment || /%2f|%5c|\\/i.test(segment)) return { problem: "The link's path has an empty or encoded-slash segment." };
    let decoded: string;
    try {
      decoded = decodeURIComponent(segment);
    } catch {
      return { problem: "The link's path is not valid percent-encoding." };
    }
    if (decoded === "." || decoded === ".." || /[\u0000-\u001f\u007f]/.test(decoded)) return { problem: "The link's path has a dot segment or a control character." };
  }
  const view = url.searchParams.get("view") ?? "inline";
  if (view !== "inline" && view !== "attachment") return { problem: `The view "${view}" is not a file's bytes (inline or attachment).` };
  return { upstream: `${settings.apiBase}${PUBLIC_PATH}${rest}?view=inline`, download: view === "attachment", fileName: originalName(decodeURIComponent(segments[segments.length - 1]!)) };
}

/** Whether a redirect from Briefcase stays on its own public files (an owner who changed their id answers 308). */
export function sameBriefcaseFile(settings: EnabledUploads, location: string, from: string): string | null {
  try {
    const next = new URL(location, from);
    const base = new URL(settings.apiBase);
    const start = `${base.pathname.replace(/\/+$/, "")}${PUBLIC_PATH}`;
    if (next.origin !== base.origin || !next.pathname.startsWith(start) || next.username || next.password) return null;
    next.search = "?view=inline";
    return next.href;
  } catch {
    return null;
  }
}

/**
 * Types the browser may show in place: raster images, audio, video and PDF. Everything else (SVG and HTML included) is
 * a download, so a file a Carbon uploaded can never run as DM: with nosniff and a sandbox CSP besides.
 */
const INLINE = /^(image\/(png|jpeg|gif|webp|avif|bmp|apng|heic|heif|x-icon|vnd\.microsoft\.icon)|audio\/[a-z0-9.+-]+|video\/[a-z0-9.+-]+|application\/pdf)$/;

export const essence = (contentType: string | null | undefined) => (contentType ?? "").split(";")[0]!.trim().toLowerCase();
export const showsInline = (contentType: string | null | undefined) => INLINE.test(essence(contentType));

function disposition(kind: "inline" | "attachment", name: string): string {
  const ascii = [...name].map(character => (character >= " " && character <= "~" && character !== '"' && character !== "\\" ? character : "_")).join("") || "file";
  const encoded = encodeURIComponent(name).replace(/['()*]/g, character => `%${character.charCodeAt(0).toString(16).toUpperCase()}`);
  return `${kind}; filename="${ascii}"; filename*=UTF-8''${encoded}`;
}

const SAFE_HEADER = /^[ -~]*$/;

/**
 * The proxy's answer headers: the bytes' own description (type, length, range, validators) and the isolation every
 * Carbon-uploaded file gets on DM's origin. Never a cookie, never a shared cache.
 *
 * A file that is not shown in place keeps its type only when it is a picture (an <img> needs image/svg+xml to draw an
 * SVG, and draws it without running anything); every other download is application/octet-stream, as Briefcase's own
 * attachment view answers. With nosniff, that means an uploaded script, stylesheet or page can never be loaded as one
 * from DM's origin, whatever a page tried.
 */
export function fileResponseHeaders(upstream: Headers, options: { download: boolean; fileName: string }): Headers {
  const out = new Headers();
  const declared = upstream.get("content-type")?.trim() ?? "";
  const valid = declared && declared.length <= 255 && MEDIA_TYPE.test(declared) && SAFE_HEADER.test(declared) ? declared : "application/octet-stream";
  const inline = !options.download && showsInline(valid);
  const type = showsInline(valid) || essence(valid).startsWith("image/") ? valid : "application/octet-stream";
  out.set("content-type", type);
  const encoded = !!upstream.get("content-encoding") && upstream.get("content-encoding")!.toLowerCase() !== "identity";
  for (const name of ["content-length", "content-range", "accept-ranges", "etag", "last-modified"]) {
    if (name === "content-length" && encoded) continue;
    const value = upstream.get(name);
    if (value !== null && SAFE_HEADER.test(value)) out.set(name, value);
  }
  const cache = upstream.get("cache-control") ?? "";
  const maxAge = /max-age=(\d+)/i.exec(cache)?.[1];
  out.set("cache-control", /no-store/i.test(cache) ? "private, no-store" : maxAge ? `private, max-age=${maxAge}` : "private, no-cache");
  out.set("x-content-type-options", "nosniff");
  out.set("content-security-policy", "sandbox; default-src 'none'");
  out.set("cross-origin-resource-policy", "same-origin");
  out.set("referrer-policy", "no-referrer");
  out.set("content-disposition", disposition(inline ? "inline" : "attachment", options.fileName));
  return out;
}
