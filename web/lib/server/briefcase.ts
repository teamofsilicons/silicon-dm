/**
 * Uploads through Silicon Briefcase, done by this website's server (spec §6.9). DM's service never handles files: the
 * website saves a Carbon's file or voice note into the Carbon's own Briefcase, in `apps/dm/`, as DM acting for the
 * Carbon with User verification proofs from Silicon Accounts, and the message carries only the public link.
 *
 *   BRIEFCASE_API_URL   Briefcase's API origin (https://api.briefcase.teamofsilicons.com). Unset: uploads are off, and
 *                       GET /api/uploads/config says so (links and GIFs still work). Plain http only for a stand-in on
 *                       this machine.
 *   UPLOADS_MAX_BYTES   optional: the largest file this website takes, in bytes, for an operator who wants a limit of
 *                       their own. Unset, the only limit is Briefcase's: 5 TiB per file.
 *
 * Files of any size pass through without being held: no temp file, no buffer. Briefcase reserves an upload by its size
 * and SHA-256 and checks the digest as the bytes arrive, so the browser hashes the file first (lib/upload/hash.ts) and
 * sends `POST /api/uploads?name=…&size=…&sha256=…` with the file as the body and its exact Content-Length. Then:
 *   1. Accounts POST /v1/proofs/user-verification (Basic APP_ID:APP_SECRET, subject_token = the session's access token,
 *      receiving_app briefcase, the five upload scopes, 600 s tokens, an Idempotency-Key). It comes first, while the
 *      access token is surely valid: the proof then lives on its own refresh token, however long the bytes take.
 *   2. Briefcase POST /api/v1/obo/uploads/reserve {operation_id, parent_path "", name, content_type, size, sha256}
 *   3. PUT /api/v1/obo/uploads/{upload_id}/content with X-Briefcase-Upload-Capability, no Authorization, and the same
 *      exact Content-Length: the browser's body is piped into it as it arrives (node:http, so the length is the
 *      header's and never chunked, and a slow side slows the other: memory stays flat whatever the size)
 *   4. POST /api/v1/obo/uploads/commit {operation_id, upload_id}
 *   5. POST /api/v1/obo/link-access {operation_id, entry_id, enabled: true}: anyone with the link can view
 *   6. Accounts POST /v1/proofs/revoke {proof_id}, whatever happened.
 * Every Briefcase request carries a proof token never sent before (Briefcase's rule: a spent proof is never retried); a
 * JSON request that failed on the way is repeated once with a fresh token and the same operation, which Briefcase
 * answers idempotently. The bytes cannot be sent twice (they were the browser's, as they went by): a transfer that broke
 * is reconciled through its status, and anything not finished is cancelled with a proof of its own, so no reservation is
 * left behind when the browser goes away, the connection breaks or Briefcase refuses. Briefcase keeps one file per path,
 * so each upload gets a name of its own (a time and a random part before the file's name).
 *
 * The link: Briefcase's public inline URL, /api/v1/public/{owner uuid}/apps/dm/{name}?view=inline. Briefcase answers
 * those bytes with Cross-Origin-Resource-Policy: same-origin, so the browser loads them through GET /api/uploads/file
 * (this module's `publicFileTarget` and `fileResponseHeaders` decide what that proxy may fetch and how it answers).
 *
 * Nothing here logs a file's contents, its name, a token, a proof or a capability.
 */
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { subscribe } from "node:diagnostics_channel";
import http, { type IncomingHttpHeaders } from "node:http";
import https from "node:https";
import { Readable, Transform } from "node:stream";
import type { ReadableStream as NodeReadableStream } from "node:stream/web";
import { pipeline } from "node:stream/promises";
import { basicAuth, type ServerEnv } from "./env";
import { errorBody } from "./session";

/* ------------------------------------------------------------------------------------------------------------------ */
/* Settings                                                                                                            */
/* ------------------------------------------------------------------------------------------------------------------ */

/** The largest file Silicon Briefcase takes (its openapi.yaml: 5 TiB). The upper bound of every upload here. */
export const BRIEFCASE_MAX_BYTES = 5 * 1024 ** 4;
const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"]);

export type UploadsSettings =
  | {
      enabled: true;
      /** The largest file taken: UPLOADS_MAX_BYTES when set (never above Briefcase's), else Briefcase's 5 TiB. */
      maxBytes: number;
      /** Whether that is an operator's limit (UPLOADS_MAX_BYTES) rather than Briefcase's own. */
      capped: boolean;
      /** Where this server calls Briefcase (no trailing slash): the origin, and a path prefix if it has one. */
      apiBase: string;
      /** What links in messages start with. The same as apiBase, except a plain-http stand-in on this machine: DM's
       * service accepts only https links, so its links are written with https and the proxy maps them back. */
      publicBase: string;
    }
  /**
   * `reason` is what Carbons read (GET /api/uploads/config, the + menu, the microphone): no setting's name, nothing an
   * operator would need. `problem` is the operator's: which setting, what is wrong with it (never its value); the
   * website's server logs it once (app/api/uploads/config/route.ts), and docs/reference/configuration.md has the rest.
   */
  | { enabled: false; maxBytes: number; capped: boolean; reason: string; problem: string };

/** What Carbons read while this website has no Briefcase to upload to. */
export const UPLOADS_NOT_SET_UP = "Uploads are not set up on this DM website yet.";
/** What Carbons read when the uploads setting is there but wrong. */
export const UPLOADS_MISCONFIGURED = "Uploads are not working on this DM website: its setup needs fixing.";

/** Reads BRIEFCASE_API_URL and UPLOADS_MAX_BYTES (at request time, like every setting of this site). */
export function uploadsSettings(source: Record<string, string | undefined> = process.env): UploadsSettings {
  const capRaw = source.UPLOADS_MAX_BYTES?.trim();
  const cap = capRaw ? Number(capRaw) : null;
  const validCap = cap === null || (Number.isSafeInteger(cap) && cap > 0);
  // A limit above Briefcase's own changes nothing: Briefcase's applies.
  const maxBytes = validCap && cap !== null ? Math.min(cap, BRIEFCASE_MAX_BYTES) : BRIEFCASE_MAX_BYTES;
  const capped = validCap && cap !== null && cap < BRIEFCASE_MAX_BYTES;
  // Neither the reason nor the problem repeats a setting's value: the problem goes to logs, and a mistaken value could
  // carry credentials.
  const off = (problem: string) => ({ enabled: false as const, maxBytes, capped, reason: UPLOADS_MISCONFIGURED, problem: `Uploads are off: ${problem}` });
  if (!validCap) return off("UPLOADS_MAX_BYTES is not a whole number of bytes (leave it unset for no limit of the website's own; 1073741824 is 1 GiB).");
  const raw = source.BRIEFCASE_API_URL?.trim();
  if (!raw) return { enabled: false, maxBytes, capped, reason: UPLOADS_NOT_SET_UP, problem: "Uploads are off: BRIEFCASE_API_URL is not set, so this website has no Silicon Briefcase to save files and voice messages into. Links and GIFs still work." };
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
  return { enabled: true, maxBytes, capped, apiBase, publicBase: loopbackHttp ? `https://${url.host}${path}` : apiBase };
}

export type EnabledUploads = Extract<UploadsSettings, { enabled: true }>;

/* ------------------------------------------------------------------------------------------------------------------ */
/* The website's own server: the deadline for a body, idle connections, stopping                                      */
/* ------------------------------------------------------------------------------------------------------------------ */

/**
 * The longest one request may take to arrive, body included, on the website's server. Node's own is 300 s (it answers
 * 408 after it): at a home connection's 100 Mbit/s that is under 4 GB, so a larger file would never arrive. A day rather
 * than no deadline at all: a body that trickles forever still ends, and no upload takes that long (Briefcase's own
 * BRIEFCASE_UPLOAD_TIMEOUT_SECONDS is far shorter). Slow headers are still cut after 60 s (`headersTimeout`), and a body
 * that stops moving is the upload's own stall timeout's business (TRANSFER_TIMING).
 */
export const REQUEST_DEADLINE_MS = 24 * 60 * 60_000;
/**
 * How long the website keeps an idle connection open. Node closes one after 5 s, and Next keeps that unless
 * KEEP_ALIVE_TIMEOUT is set. A load balancer that keeps its own longer (production's: 180 s) then sends a request now and
 * then on a connection the website is just closing, and answers it 502 (an upload's included, at its first byte): the
 * website's must outlast the balancer's.
 */
export const KEEP_ALIVE_MS = 185_000;
/** Node's own keep-alive, what Next leaves in place when KEEP_ALIVE_TIMEOUT is not set. */
const NODE_KEEP_ALIVE_MS = 5_000;
const PREPARED = Symbol.for("dm.uploads.server");

/**
 * Sets up the website's server for uploads of any length: instrumentation.ts runs it when the server starts, before it
 * answers anything (Next loads a route's code only when the route's first request arrives, too late to change how that
 * request is timed). Next creates its server with Node's defaults and offers no setting for the deadline, so this listens
 * on the `http.server.request.start` diagnostics channel, whose message carries the server, and on the first request,
 * whatever its path, sets the whole server's REQUEST_DEADLINE_MS (Node has one per server and reads it at each check, so
 * a request already under way is covered as well) and, unless KEEP_ALIVE_TIMEOUT says otherwise, KEEP_ALIVE_MS. It also
 * stops the uploads under way when the process is told to stop (stopUploads). Once per process.
 */
export function prepareServer(source: Record<string, string | undefined> = process.env): void {
  const scope = globalThis as { [PREPARED]?: boolean };
  if (scope[PREPARED]) return;
  scope[PREPARED] = true;
  const prepared = new WeakSet<object>();
  subscribe("http.server.request.start", message => {
    const { server } = message as { server?: http.Server };
    if (!server || prepared.has(server)) return;
    prepared.add(server);
    if (server.requestTimeout > 0 && server.requestTimeout < REQUEST_DEADLINE_MS) server.requestTimeout = REQUEST_DEADLINE_MS;
    // An operator's own keep-alive (KEEP_ALIVE_TIMEOUT, which Next applies itself) stays as it is.
    if (!source.KEEP_ALIVE_TIMEOUT?.trim() && server.keepAliveTimeout === NODE_KEEP_ALIVE_MS) server.keepAliveTimeout = KEEP_ALIVE_MS;
  });
  const onSignal = (signal: NodeJS.Signals) => {
    const stopped = stopUploads();
    // Node's own exit on a signal is off while anything listens for it. Next listens (and exits once its requests and
    // after() work are done, these uploads' included); a server without such a listener ends here, once they are let go.
    if (process.listenerCount(signal) <= 1) void stopped.finally(() => process.exit(signal === "SIGINT" ? 130 : 143));
  };
  process.on("SIGTERM", onSignal);
  process.on("SIGINT", onSignal);
}

/** The server is stopping (a deploy or a restart): the file was not saved, and the page sends it again by itself. */
export const restarting = () =>
  new UploadFailure(503, "website_restarting", "This DM website is restarting, so the file was not saved.", "Send it again in a moment, from its first byte (this website's pages do it by themselves).", 10);

interface UploadsRuntime {
  stopping: boolean;
  /**
   * Every upload this process is working on, from its proof to its outcome (the save after a 202 included). `stop`
   * stops it with a failure made by the copy of this module that started it: instrumentation.ts and the routes are
   * bundled apart, each with its own copy, and a failure made by one is not an instance of the other's class.
   */
  running: Set<{ stop: () => void; ended: Promise<void> }>;
}
const RUNTIME = Symbol.for("dm.uploads.runtime");
/** On the global object, so a module loaded twice (a dev server's reload, two route bundles) shares it. */
const runtime: UploadsRuntime = ((globalThis as { [RUNTIME]?: UploadsRuntime })[RUNTIME] ??= { stopping: false, running: new Set() });

/** Whether this server is stopping (it starts no upload). */
export const stopping = () => runtime.stopping;

/** Counts an upload as under way until `work` settles; `stop` stops it if the server stops first. */
export function trackUpload(stop: AbortController, work: Promise<unknown>): void {
  const entry = { stop: () => stop.abort(restarting()), ended: work.then(() => undefined, () => undefined) };
  runtime.running.add(entry);
  void entry.ended.then(() => runtime.running.delete(entry));
}

/**
 * The process is told to stop (a deploy's SIGTERM, or Ctrl-C): no new upload starts, and every one under way stops now,
 * with its reservation at Briefcase cancelled with a proof token of its own. Left alone, a reservation would stay
 * pending at Briefcase for up to a day, counted against the Carbon's daily allowance and their 32 unfinished uploads,
 * because nothing in a new process knows it. Next waits for open requests before it exits, and for a save that goes on
 * after a 202 through after() (app/api/uploads/route.ts); a stop that kills the process sooner (docker stop's 10 s)
 * cuts only what Briefcase had not answered by then. Resolves once every one has ended.
 */
export function stopUploads(): Promise<void> {
  runtime.stopping = true;
  const running = [...runtime.running];
  if (running.length) console.warn(`dm: stopping; ${running.length === 1 ? "one upload was" : `${running.length} uploads were`} under way and ${running.length === 1 ? "its reservation is" : "their reservations are"} being cancelled at Silicon Briefcase`);
  for (const upload of running) upload.stop();
  return Promise.all(running.map(upload => upload.ended)).then(() => undefined);
}

/* ------------------------------------------------------------------------------------------------------------------ */
/* Failures in the words a Carbon reads                                                                                */
/* ------------------------------------------------------------------------------------------------------------------ */

/** A failed upload, answered as `{"error": {code, message, hint}}` with its status (and Retry-After when it has one). */
export class UploadFailure extends Error {
  readonly status: number;
  readonly code: string;
  readonly hint: string | undefined;
  readonly retryAfter: number | undefined;
  /** What Silicon Briefcase or Silicon Accounts itself answered (its own error code, or "HTTP 503"), for the server's log. */
  upstream: string | undefined;
  /** Briefcase did not let go of this upload's reservation (its cancel was refused or not answered): it expires there. */
  reservationLeft = false;

  constructor(status: number, code: string, message: string, hint?: string, retryAfter?: number, upstream?: string) {
    super(message);
    this.name = "UploadFailure";
    this.status = status;
    this.code = code;
    this.hint = hint;
    this.retryAfter = retryAfter;
    this.upstream = upstream;
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

/**
 * What an upload ended with, as the failure the browser hears. The server's log gets what an operator can act on: this
 * website's own errors, and refusals that point at Briefcase's or Silicon Accounts' setup or availability (5xx: its
 * code, and theirs, such as `briefcase_not_allowed (proof_scope_missing)`), never a file's name, a token or a proof. A
 * Carbon's own refusals (too large, changed, too many at once) are not logged.
 */
export function failureOf(error: unknown, when = ""): UploadFailure {
  const failure = UploadFailure.from(error);
  const what = `upload failed${when ? ` ${when}` : ""}`;
  if (!(error instanceof UploadFailure)) console.error(`${what}: ${error instanceof Error ? `${error.name}: ${error.message}` : String(error)}`);
  else if (failure.status >= 500 && failure.code !== "website_restarting") console.warn(`${what}: ${failure.code}${failure.upstream ? ` (${failure.upstream})` : ""}`);
  return failure;
}

const KB = 1024;
const MB = 1024 * KB;
const GB = 1024 * MB;
const TB = 1024 * GB;

/** "640 KB", "12 MB", "4.8 GB", "5 TB" (a decimal only under 10). */
export const formatBytes = (bytes: number) => {
  const [unit, size] = bytes >= TB ? ["TB", TB] as const : bytes >= GB ? ["GB", GB] as const : bytes >= MB ? ["MB", MB] as const : bytes >= KB ? ["KB", KB] as const : ["bytes", 1] as const;
  const value = bytes / size;
  return `${size === 1 || value >= 10 ? Math.round(value) : Number(value.toFixed(1))} ${unit}`;
};

export const tooLarge = (maxBytes: number, capped: boolean) =>
  new UploadFailure(
    413,
    "file_too_large",
    capped ? `The file is larger than ${formatBytes(maxBytes)}, the most this DM website takes.` : `The file is larger than ${formatBytes(maxBytes)}, the most Silicon Briefcase takes in one file.`,
    capped ? "Make it smaller, or share a link to it instead (+\u00a0→\u00a0Add a link)." : "Split it into smaller files, or share a link to it instead (+\u00a0→\u00a0Add a link).",
  );

/** The page that sent this upload is older than this website (it does not hash files first): it reloads. */
export const pageOutdated = () =>
  new UploadFailure(409, "page_outdated", "This page is older than the DM website, so it cannot upload this way.", "Reload the page, then send the file again.");

/**
 * Uploads one account may have on this server at once. A tab sends three at a time (the composer's tray), so this
 * leaves room for two tabs; each holds a connection to the browser and one to Briefcase for as long as its bytes take.
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

/** The browser went away (or stopped sending) before the last byte: nothing was saved, and nobody is listening. */
export const interrupted = () =>
  new UploadFailure(400, "upload_interrupted", "The file did not arrive completely, so nothing was saved.", "Check the connection, then send it again (a file starts again from its first byte).");

/* ------------------------------------------------------------------------------------------------------------------ */
/* The file: name, type, size, digest                                                                                  */
/* ------------------------------------------------------------------------------------------------------------------ */

/** Briefcase's longest entry name, in bytes of UTF-8 (its EntryName: 255 bytes, not characters). */
const BRIEFCASE_NAME_MAX_BYTES = 255;
/** "20261017-153212-k3f9a2-": what storedName puts before the file's own name (ASCII: a byte a character). */
const PREFIX_LENGTH = 23;
const STORED = /^\d{8}-\d{6}-[0-9a-z]{6}-(.+)$/;

const utf8 = new TextEncoder();
const bytesOf = (text: string) => utf8.encode(text).length;

/** The characters a person sees (a letter with its accents, an emoji with its modifiers), so a cut never splits one. */
function graphemes(text: string): string[] {
  if (typeof Intl.Segmenter !== "function") return [...text];
  return Array.from(new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text), part => part.segment);
}

/**
 * Shortens a name to at most `maxBytes` bytes of UTF-8, keeping its extension and whole characters. Bytes, not
 * characters: Briefcase measures names that way, so a name in Japanese or Hindi (three bytes a character) or with emoji
 * (four) is cut where Briefcase would refuse it, not after.
 */
function shorten(name: string, maxBytes: number): string {
  if (bytesOf(name) <= maxBytes) return name;
  const dot = name.lastIndexOf(".");
  const extension = dot > 0 && name.length - dot <= 16 ? name.slice(dot) : "";
  const room = maxBytes - bytesOf(extension);
  let stem = "";
  let used = 0;
  for (const part of graphemes(name.slice(0, name.length - extension.length))) {
    const size = bytesOf(part);
    if (used + size > room) break;
    stem += part;
    used += size;
  }
  return `${stem.trimEnd()}${extension}`;
}

/**
 * Characters that change how the rest of a name is drawn without being seen: bidirectional overrides and isolates (a
 * right-to-left override turns "invoice‮gnp.exe" into "invoiceexe.png" on screen) and the directional marks.
 */
const INVISIBLE_DIRECTION = /[؜‎‏‪-‮⁦-⁩]/g;

/**
 * The name a person sees: the file's own name with folders, control characters (C0 and C1: Briefcase trims U+0085 as
 * a space, which would leave the link naming a file it does not have), invisible direction changes and runs of spaces
 * taken out, short enough in bytes that Briefcase takes it with storedName's prefix. Null when nothing usable is left.
 */
export function cleanFileName(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const name = raw.normalize("NFC").replace(/[\u0000-\u001f\u007f-\u009f]/g, "").replace(INVISIBLE_DIRECTION, "").replace(/[\\/]/g, "-").replace(/\s+/g, " ").trim();
  if (!name || name === "." || name === "..") return null;
  const short = shorten(name, BRIEFCASE_NAME_MAX_BYTES - PREFIX_LENGTH);
  return short && short !== "." && short !== ".." ? short : null;
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

/** What the browser says about the file it is sending: its size and SHA-256 (from the query), checked. */
export interface Declared {
  size: number;
  sha256: string;
}

/**
 * The size and digest the browser declared, and the request's own framing: an exact Content-Length equal to the size
 * (Briefcase takes exactly that many bytes and no chunked transfer), never a compressed body.
 */
export function declaredFile(query: URLSearchParams, headers: Headers, settings: { maxBytes: number; capped: boolean }): Declared | UploadFailure {
  const sizeRaw = query.get("size");
  const sha256 = query.get("sha256")?.toLowerCase() ?? "";
  if (sizeRaw === null && !query.has("sha256")) return pageOutdated();
  if (!sizeRaw || !/^\d{1,16}$/.test(sizeRaw)) return new UploadFailure(400, "invalid_size", "The upload did not say the file's size in bytes: POST /api/uploads?name=…&size=<bytes>&sha256=<hex>.", "Reload the page, then send the file again.");
  if (!/^[0-9a-f]{64}$/.test(sha256)) return new UploadFailure(400, "invalid_digest", "The upload did not carry the file's SHA-256 (64 hex characters), which Silicon Briefcase checks the bytes against.", "Reload the page, then send the file again.");
  const size = Number(sizeRaw);
  if (size === 0) return new UploadFailure(422, "file_empty", "This file is empty (0 bytes), so there is nothing to send.", "Choose another file.");
  if (size > settings.maxBytes) return tooLarge(settings.maxBytes, settings.capped);
  if (headers.get("transfer-encoding") || !headers.has("content-length")) {
    return new UploadFailure(411, "length_required", "The upload did not say its exact length (it was sent in chunks), and Silicon Briefcase takes a file only with its exact length.", "Send the file itself as the body, with its Content-Length (a browser does this by itself).");
  }
  if (headers.get("content-length") !== sizeRaw) {
    return new UploadFailure(400, "size_mismatch", "The upload's length is not the size it declared for the file.", "Send the file again.");
  }
  const encoding = headers.get("content-encoding");
  if (encoding && encoding.toLowerCase() !== "identity") return new UploadFailure(415, "encoded_body", "The upload's body was compressed, and the file must arrive as its own bytes.", "Send the file itself, uncompressed.");
  return { size, sha256 };
}

/**
 * After a refusal answered before the body was read: keeps reading (and dropping) what the browser still sends for a
 * moment, so that it hears the answer rather than a closed connection, then lets go (the connection closes). Never
 * the whole of a big file.
 */
export function discardBody(body: ReadableStream<Uint8Array> | null, limit: { bytes: number; ms: number } = { bytes: 32 * MB, ms: 10_000 }): void {
  if (!body || body.locked) return;
  const reader = body.getReader();
  const stop = () => void reader.cancel().catch(() => undefined);
  const timer = setTimeout(stop, limit.ms);
  void (async () => {
    let read = 0;
    try {
      while (read < limit.bytes) {
        const { done, value } = await reader.read();
        if (done) return;
        read += value.byteLength;
      }
      stop();
    } catch {
      // The browser went away: nothing left to answer.
    } finally {
      clearTimeout(timer);
    }
  })();
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

function parseBody(text: string): Record<string, unknown> | null {
  try {
    const value: unknown = text ? JSON.parse(text) : null;
    return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

async function readAnswer(response: Response): Promise<Answer> {
  const text = await response.text().catch(() => "");
  return { status: response.status, body: parseBody(text), headers: response.headers };
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
    return new UploadFailure(503, "briefcase_unavailable", `Silicon Briefcase is not available to DM right now (Silicon Accounts: ${code}).`, "Send a link instead (+\u00a0→\u00a0Add a link), or try again later.");
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
  return new UploadFailure(409, "permission_ended", `DM's permission to save this file ended before it was saved${code ? ` (${code})` : ""}.`, "Sign in again if you signed out, then send the file again.");
}

/**
 * A User verification proof for one upload: DM acting for the signed-in Carbon at Briefcase. `fresh()` hands out a
 * proof token that was never sent before (the issued one while it has a minute left, then one new token per call), and
 * keeps only the newest refresh token (a used one presented again would end the proof). A proof lives as long as the
 * Carbon's sign-in, so a transfer of any length can still be committed after its last byte.
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
/* Briefcase: the JSON routes                                                                                          */
/* ------------------------------------------------------------------------------------------------------------------ */

const BRIEFCASE_TIMEOUT_MS = 60_000;

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

/**
 * What Silicon Briefcase takes into one Briefcase in a UTC day unless its operators gave that one more (its
 * DEFAULT_DAILY_UPLOAD_LIMIT_BYTES, 100 GiB). Unfinished uploads count toward it too, so a larger file is refused at its
 * reservation on any day. DM cannot see a Briefcase's own allowance, so it never refuses a file for it: the composer
 * says so when such a file is added, and a refusal says which case it is.
 */
export const BRIEFCASE_DAILY_UPLOAD_BYTES = 100 * 1024 ** 3;

/** Briefcase's own answer in a few words, for the server's log: its error code, else its HTTP status. */
const upstreamOf = (answer: Answer) => errorOf(answer).code || (answer.status ? `HTTP ${answer.status}` : "no answer");

/** What a refusal is about, when the step knows it: the file's size, for words that fit the case. */
export interface RefusedFile {
  size?: number;
}

/** Why Briefcase did not take the file, as the browser hears it (and, for the server's log, what Briefcase said). */
export function briefcaseRefused(answer: Answer, step: string, file: RefusedFile = {}): UploadFailure {
  const failure = refusalFor(answer, step, file);
  failure.upstream ??= upstreamOf(answer);
  return failure;
}

function refusalFor(answer: Answer, step: string, file: RefusedFile): UploadFailure {
  if (transient(answer.status)) return unavailable("Silicon Briefcase", retryAfterOf(answer));
  const { code, message } = errorOf(answer);
  if (["proof_issuer_not_allowed", "proof_scope_missing", "proof_kind_not_accepted"].includes(code)) {
    return new UploadFailure(503, "briefcase_not_allowed", "Silicon Briefcase does not accept files from DM yet.", "Send a link instead (+\u00a0→\u00a0Add a link). Briefcase's operators allow DM in BRIEFCASE_PROOF_ISSUERS.");
  }
  if (answer.status === 507) return new UploadFailure(507, "briefcase_full", "Your Silicon Briefcase is full, so the file was not saved.", "Free some space in Silicon Briefcase, then try again.");
  if (answer.status === 429 && code === "daily_upload_limit_exhausted") {
    // Briefcase says only that the day has no room. A file larger than the usual allowance has none on any day, so
    // waiting for midnight would not help it: say that, not "today".
    if ((file.size ?? 0) > BRIEFCASE_DAILY_UPLOAD_BYTES) {
      return new UploadFailure(429, "briefcase_limit", `This file is larger than ${formatBytes(BRIEFCASE_DAILY_UPLOAD_BYTES)}, the most Silicon Briefcase takes into one Briefcase in a day, so it was not saved.`, "Split it into parts smaller than that, or share a link to it instead (+\u00a0→\u00a0Add a link). Briefcase's operators can give a Briefcase a larger allowance.");
    }
    return new UploadFailure(429, "briefcase_limit", "Today's upload allowance in your Silicon Briefcase has no room left for this file: what you uploaded today counts, and so do uploads not yet finished.", "It resets at 00:00 UTC, and an unfinished upload stops counting once it ends; until then, send a link instead (+\u00a0→\u00a0Add a link).", retryAfterOf(answer));
  }
  if (code === "delegated_upload_staging_limit_exhausted") {
    return new UploadFailure(429, "briefcase_busy", "Silicon Briefcase is still holding other unfinished uploads of yours, so it did not take another one.", "Wait for your other uploads to finish (or remove them), then retry this one.", 30);
  }
  if (answer.status === 429) {
    // Briefcase asked DM to slow down: a limit of Briefcase's, not of the Carbon's.
    return new UploadFailure(503, "briefcase_unavailable", "Silicon Briefcase is busy and asked DM to wait, so the file was not saved.", "Try again in a moment.", retryAfterOf(answer) ?? 5);
  }
  if (answer.status === 413) return new UploadFailure(413, "file_too_large", "Silicon Briefcase does not take a file this large.", "Split it into smaller files, or share a link to it instead (+\u00a0→\u00a0Add a link).");
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
async function delegated(settings: EnabledUploads, proof: UploadProof, route: string, payload: unknown, step: string, file: RefusedFile = {}): Promise<Answer> {
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
    if (attempt === 0) await new Promise(resolve => setTimeout(resolve, 500));
  }
  throw briefcaseRefused(answer, step, file);
}

/**
 * Cancels a reservation with a proof token of its own (cancelling never touches a published file). Answers whether
 * Briefcase let it go. When it did not (it refused, or did not answer), the reservation stays pending there until it
 * expires (within a day), counted against the Carbon's daily allowance meanwhile: the server's log says so, with
 * Briefcase's own code and never the file's name, and the caller tells the browser the truth.
 */
async function cancelReservation(settings: EnabledUploads, proof: UploadProof, operation: string): Promise<boolean> {
  try {
    const answer = (await delegated(settings, proof, "uploads/cancel", { operation_id: operation }, "cancel")).body as UploadState | null;
    // A reservation that had already ended (expired, cancelled) is let go as well; one still taking bytes is not.
    if (!["reserved", "receiving", "staged"].includes(answer?.state ?? "")) return true;
    console.warn(`upload: Silicon Briefcase answered a cancel but still holds the upload (${answer?.state}); it expires there within a day`);
    return false;
  } catch (error) {
    const why = error instanceof UploadFailure ? (error.upstream ?? error.code) : error instanceof Error ? error.name : "unknown";
    console.warn(`upload: Silicon Briefcase did not cancel an unfinished upload (${why}); it stays reserved there until it expires, within a day`);
    return false;
  }
}

/* ------------------------------------------------------------------------------------------------------------------ */
/* Briefcase: the bytes                                                                                                */
/* ------------------------------------------------------------------------------------------------------------------ */

export interface TransferTiming {
  /** How long nothing may move (no byte from the browser, none taken by Briefcase) before the transfer stops. */
  stallMs: number;
  /** After the last byte, how long Briefcase may take to answer (it checks the digest and stores the file first). */
  answerMs: number;
  /** How long to wait for Briefcase's "100 Continue" before sending the bytes anyway (a proxy may never pass one on). */
  continueMs: number;
}

export const TRANSFER_TIMING: TransferTiming = { stallMs: 5 * 60_000, answerMs: 45 * 60_000, continueMs: 1_500 };

export type TransferOutcome =
  /** Briefcase answered (200 with the upload staged, or a refusal). `started`: whether any byte was sent. */
  | { kind: "answered"; answer: Answer; received: number; started: boolean }
  /** The browser went away, or its body ended or broke before the last byte. */
  | { kind: "browser_gone"; received: number }
  /** Nothing moved for `stallMs`. */
  | { kind: "stalled"; received: number }
  /** The connection to Briefcase broke, or its answer did not come in time: where the upload stands is unknown. */
  | { kind: "lost"; received: number };

const answerHeaders = (headers: IncomingHttpHeaders) => {
  const out = new Headers();
  for (const [name, value] of Object.entries(headers)) if (typeof value === "string") out.set(name, value);
  return out;
};

/**
 * Pipes the browser's body into Briefcase's transfer route: one PUT with the reservation's capability alone, the exact
 * Content-Length (node:http sends the header as given and never switches to chunks; `strictContentLength` makes it
 * refuse to send a byte more or less), and backpressure end to end (a slow Briefcase slows the browser; a slow browser,
 * Briefcase). Exactly `size` bytes must pass; more or fewer is the browser's failure.
 *
 * The request asks `Expect: 100-continue` and sends the bytes on Briefcase's 100 (or after `continueMs` without one):
 * a refusal decided from the headers (the reservation gone, its time too short) then arrives before a byte has left,
 * instead of being lost to a connection Briefcase closes while the bytes still pour in; and the browser's body is not
 * touched until then, so the website can still answer the browser in words. Resolves with what happened; never rejects.
 * `onSent` is told when the last byte has gone (the browser's body is all read; Briefcase is checking and storing it).
 */
export function sendBytes(url: string, capability: string, size: number, body: ReadableStream<Uint8Array>, signal?: AbortSignal, timing: TransferTiming = TRANSFER_TIMING, onSent?: () => void): Promise<TransferOutcome> {
  return new Promise(resolve => {
    const target = new URL(url);
    const transport = target.protocol === "https:" ? https : http;
    let received = 0;
    let settled = false;
    let answering = false;
    let browserFailed = false;
    let source: Readable | null = null;
    let sentAll = false;
    let lastMove = Date.now();
    const outgoing = transport.request(target, {
      method: "PUT",
      // A connection of its own for as long as the bytes take.
      agent: false,
      headers: {
        "content-type": "application/octet-stream",
        "content-length": String(size),
        "x-briefcase-upload-capability": capability,
        accept: "application/json",
        expect: "100-continue",
      },
    });
    // On every outgoing message since Node 18.10 (its types list it only for responses).
    (outgoing as http.ClientRequest & { strictContentLength: boolean }).strictContentLength = true;

    const settle = (outcome: TransferOutcome) => {
      if (settled) return;
      settled = true;
      signal?.removeEventListener("abort", onAbort);
      clearInterval(watch);
      clearTimeout(waitForContinue);
      if (outcome.kind !== "answered" || !sentAll) {
        source?.destroy();
        outgoing.destroy();
      }
      resolve(outcome);
    };
    const onAbort = () => settle({ kind: "browser_gone", received });
    signal?.addEventListener("abort", onAbort, { once: true });

    // The stall watch: while bytes are on their way, nothing moving for stallMs ends the transfer; after the last byte,
    // Briefcase has answerMs to answer.
    const watch = setInterval(() => {
      const quiet = Date.now() - lastMove;
      if (!sentAll && quiet > timing.stallMs) settle({ kind: "stalled", received });
      else if (sentAll && !answering && quiet > timing.answerMs) settle({ kind: "lost", received });
    }, Math.max(50, Math.min(5_000, timing.stallMs / 4)));
    watch.unref?.();

    outgoing.on("response", response => {
      answering = true;
      // An answer before the last byte is a refusal: stop taking what the browser sends.
      if (!sentAll) source?.destroy();
      const chunks: Buffer[] = [];
      let length = 0;
      response.on("data", (chunk: Buffer) => {
        if (length < 64 * KB) {
          chunks.push(chunk);
          length += chunk.length;
        }
      });
      response.on("end", () => settle({ kind: "answered", received, started: source !== null, answer: { status: response.statusCode ?? 0, body: parseBody(Buffer.concat(chunks).toString("utf8")), headers: answerHeaders(response.headers) } }));
      response.on("error", () => settle({ kind: "lost", received }));
    });
    outgoing.on("error", () => {
      if (!answering) settle(browserFailed ? { kind: "browser_gone", received } : { kind: "lost", received });
    });
    // Writes Briefcase takes count as movement too (a slow Briefcase is not a stall while it keeps taking bytes).
    outgoing.on("drain", () => {
      lastMove = Date.now();
    });

    const start = () => {
      if (source || settled || answering) return;
      clearTimeout(waitForContinue);
      lastMove = Date.now();
      const from = Readable.fromWeb(body as unknown as NodeReadableStream<Uint8Array>);
      source = from;
      from.on("error", () => {
        browserFailed = true;
      });
      const meter = new Transform({
        transform(chunk: Buffer, _encoding, done) {
          received += chunk.length;
          lastMove = Date.now();
          if (received > size) {
            browserFailed = true;
            return done(new Error("more bytes than the upload declared"));
          }
          done(null, chunk);
        },
        flush(done) {
          if (received === size) return done();
          browserFailed = true;
          done(new Error("fewer bytes than the upload declared"));
        },
      });
      pipeline(from, meter, outgoing).then(
        () => {
          sentAll = true;
          lastMove = Date.now();
          if (!settled) onSent?.();
        },
        () => {
          if (answering || settled) return;
          settle(browserFailed || signal?.aborted ? { kind: "browser_gone", received } : { kind: "lost", received });
        },
      );
    };
    outgoing.on("continue", start);
    const waitForContinue = setTimeout(start, timing.continueMs);
    if (signal?.aborted) return onAbort();
    outgoing.flushHeaders();
  });
}

/** Briefcase's refusal of the bytes themselves, as the browser hears it. */
function transferRefused(answer: Answer): UploadFailure {
  const { code } = errorOf(answer);
  if (code === "delegated_upload_manifest_mismatch") {
    return new UploadFailure(409, "file_changed", "The file's bytes are not the ones prepared for it: it changed on its way or after it was chosen, so it was not saved.", "Choose the file again and send it.");
  }
  if (code === "delegated_upload_insufficient_lifetime") {
    return new UploadFailure(503, "briefcase_unavailable", "Silicon Briefcase's reservation for this file ran out of time before the bytes could start.", "Send the file again.");
  }
  if (code === "delegated_upload_not_reserved" || answer.status === 404) {
    return new UploadFailure(503, "briefcase_unavailable", "Silicon Briefcase was no longer waiting for this file's bytes (its reservation had closed), so it was not saved.", "Send the file again.");
  }
  return briefcaseRefused(answer, "transfer");
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

export interface SaveInput {
  settings: EnabledUploads;
  proof: UploadProof;
  /** The stored name (storedName). */
  name: string;
  contentType: string;
  /** What the browser declared (declaredFile). */
  file: Declared;
  /** The browser's body: the file's bytes, piped through as they arrive. */
  body: ReadableStream<Uint8Array>;
  /**
   * Aborts when the browser goes away, when the save is cancelled or abandoned after its last byte, or when the server
   * stops (the reason is then the UploadFailure the browser would hear: `cancelled`, `abandoned`, `restarting`).
   */
  signal?: AbortSignal;
  /** The Carbon's account uuid: links name the owner by uuid, which never changes (an id can). */
  owner: string;
  appId: string;
  timing?: TransferTiming;
  /** Told when the browser's last byte has gone to Briefcase (POST /api/uploads may then answer 202 and let it save). */
  onTransferred?: () => void;
}

/** Why a save was stopped by this website (cancelled, abandoned, restarting), when the abort says; else null (the browser left). */
function stopReason(signal: AbortSignal | undefined): UploadFailure | null {
  if (!signal?.aborted) return null;
  const reason: unknown = signal.reason;
  if (reason instanceof UploadFailure) return reason;
  // One made by another copy of this module (instrumentation.ts is bundled apart from the routes): the same words.
  if (reason instanceof Error && reason.name === "UploadFailure") {
    const { status, code, hint, retryAfter } = reason as UploadFailure;
    if (typeof status === "number" && typeof code === "string") return new UploadFailure(status, code, reason.message, hint, retryAfter);
  }
  return null;
}

/**
 * Reserve → transfer (the browser's bytes piped through) → commit → link-access. Whatever stops it before the commit
 * (the browser leaving, a stall, a refusal, Briefcase out of reach, the server stopping) cancels the reservation with a
 * proof token of its own, so nothing is left reserved (when Briefcase does not let go, the failure says so:
 * `reservationLeft`); a transfer whose outcome is unknown is first reconciled through its status (the bytes may all
 * have arrived).
 */
export async function saveToBriefcase(input: SaveInput): Promise<{ url: string }> {
  const { settings, proof, name, contentType, file, owner, appId } = input;
  // Stopped before anything was asked of Briefcase (the server is stopping, or the browser already left).
  if (input.signal?.aborted) throw stopReason(input.signal) ?? interrupted();
  const operation = randomUUID();
  const manifest = { operation_id: operation, parent_path: "", name, content_type: contentType, size: file.size, sha256: file.sha256 };
  let reserved = false;
  let committed = false;
  try {
    const reservation = (await delegated(settings, proof, "uploads/reserve", manifest, "reserve", { size: file.size })).body as UploadState;
    reserved = true;
    if (reservation.state !== "reserved" || !reservation.upload_id || !reservation.capability) {
      throw new UploadFailure(502, "briefcase_refused", `Silicon Briefcase did not open the upload (it is ${reservation.state ?? "in an unknown state"}).`, "Try again.");
    }
    const transferUrl = `${settings.apiBase}/api/v1/obo/uploads/${encodeURIComponent(reservation.upload_id)}/content`;
    const outcome = await sendBytes(transferUrl, reservation.capability, file.size, input.body, input.signal, input.timing, input.onTransferred);
    let upload: UploadState = reservation;
    if (outcome.kind === "browser_gone") throw stopReason(input.signal) ?? interrupted();
    if (outcome.kind === "answered") {
      if (outcome.answer.status !== 200) throw transferRefused(outcome.answer);
      upload = { ...reservation, ...(outcome.answer.body as UploadState | null) };
    } else {
      // The connection to Briefcase broke or went quiet: every byte may still have arrived. Ask where it stands.
      upload = (await delegated(settings, proof, "uploads/status", { operation_id: operation }, "status")).body as UploadState;
      if (upload.state !== "staged" && upload.state !== "committed") {
        throw outcome.kind === "stalled"
          ? new UploadFailure(408, "upload_stalled", "The upload stalled: nothing moved for five minutes, so it was stopped and nothing was saved.", "Check the connection, then send the file again (it starts again from its first byte).")
          : new UploadFailure(503, "briefcase_unavailable", "Silicon Briefcase stopped taking the file part way, so it was not saved.", "Try again in a moment (the file starts again from its first byte).", 5);
      }
    }
    // A browser that left while the last bytes were checked (or a save cancelled or abandoned meanwhile): nothing would
    // use the file, so it is not published.
    if (input.signal?.aborted) throw stopReason(input.signal) ?? interrupted();
    if (upload.state === "staged") {
      upload = (await delegated(settings, proof, "uploads/commit", { operation_id: operation, upload_id: upload.upload_id ?? reservation.upload_id }, "commit")).body as UploadState;
    }
    if (upload.state !== "committed" || !upload.published_entry_id) {
      throw new UploadFailure(502, "briefcase_refused", `Silicon Briefcase did not publish the file (it is ${upload.state ?? "in an unknown state"}).`, "Try again.");
    }
    committed = true;
    let link: { url?: unknown; effective?: unknown };
    try {
      link = (await delegated(settings, proof, "link-access", { operation_id: randomUUID(), entry_id: upload.published_entry_id, enabled: true }, "link")).body as { url?: unknown; effective?: unknown };
    } catch (error) {
      throw linkFailed(error instanceof UploadFailure ? error : null, `apps/${appId}/${name}`);
    }
    if (link.effective !== true) throw linkFailed(null, `apps/${appId}/${name}`);
    return { url: publicFileUrl(settings, owner, pathFromLink(link.url, name) ?? ["apps", appId, name]) };
  } catch (error) {
    // Anything not committed is let go at Briefcase; when Briefcase does not let go, the browser is told the truth.
    if (reserved && !committed && !(await cancelReservation(settings, proof, operation)) && error instanceof UploadFailure) error.reservationLeft = true;
    throw error;
  }
}

/**
 * The file is saved (committed) but its link did not open: what the browser hears. Never "not saved", and the copy is
 * named by its place in the Carbon's Briefcase, since each attempt saves one of its own. A refusal that sending again
 * cannot change (Briefcase not letting DM open links: `link_access.update` missing from its BRIEFCASE_PROOF_ISSUERS for
 * dm) says so instead of asking for a retry; one that can pass (Briefcase or Silicon Accounts out of reach) asks for one.
 * The server's log gets Briefcase's own code (failureOf), never the file's name.
 */
function linkFailed(why: UploadFailure | null, place: string): UploadFailure {
  const copy = `this copy stays in your Briefcase as ${place}`;
  if (why?.code === "briefcase_not_allowed") {
    return new UploadFailure(503, "briefcase_not_allowed", "Silicon Briefcase saved the file but does not let DM open links to files yet, so it was not added to the message.", `Share it from Silicon Briefcase instead (${copy}); sending it again would only save another copy. Briefcase's operators allow DM in BRIEFCASE_PROOF_ISSUERS.`, undefined, why.upstream);
  }
  if (why?.code === "briefcase_unavailable" || why?.code === "accounts_unavailable") {
    return new UploadFailure(503, why.code, "Silicon Briefcase saved the file but could not open a link to it just now, so it was not added to the message.", `Try again in a moment: it uploads again, and ${copy}.`, why.retryAfter, why.upstream);
  }
  if (why?.code === "signed_out" || why?.code === "permission_ended") {
    return new UploadFailure(why.status, why.code, "Silicon Briefcase saved the file, but DM's permission to act for you ended before it could open a link to it, so it was not added to the message.", `Sign in again if you signed out, then send it again (${copy}).`, undefined, why.upstream);
  }
  return new UploadFailure(502, "briefcase_refused", `Silicon Briefcase saved the file but did not open a link to it${why?.upstream ? ` (${why.upstream})` : ""}, so it was not added to the message.`, `Try again: it uploads again, and ${copy}. If it keeps failing, report it in Settings → Report a problem.`, undefined, why?.upstream);
}

/* ------------------------------------------------------------------------------------------------------------------ */
/* Saving after the last byte (POST /api/uploads answers 202; GET|DELETE /api/uploads/saving)                          */
/* ------------------------------------------------------------------------------------------------------------------ */

/**
 * After a file's last byte, Briefcase checks it and stores it before it answers: for a large file that takes minutes,
 * with nothing moving on either connection. An ingress cuts a request that stays silent for its idle timeout
 * (production's load balancer: 180 s) and the browser would hear 504 for a file that was about to be saved, so POST
 * /api/uploads never waits silently that long: when Briefcase has not answered `holdMs` after the last byte, it answers
 * 202 with an id and the save goes on here, holding the account's upload slot and the proof until it ends. The browser
 * then asks GET /api/uploads/saving?id=… (each ask waits up to `waitMs` for the outcome) until it has the link; DELETE
 * cancels the save, and a page that goes away sends that DELETE as it goes. One nobody asks about for `abandonMs` (a
 * browser that crashed) is cancelled too, as a transfer is when its browser goes away: nothing would use the file. The
 * wait is long because the bytes are all at Briefcase by then: a laptop closed for a few minutes, or a phone changing
 * networks, still gets its file. An outcome is kept for `keepMs`, longer than that. All of it lives in this server's
 * memory, so one website process takes a Carbon's uploads (production runs one; with several, keep a Carbon on one).
 */
export const SAVING_TIMING = { holdMs: 20_000, waitMs: 25_000, abandonMs: 10 * 60_000, keepMs: 15 * 60_000 };

/** What the browser gets for a saved file: the link a message carries, and what the website saved it as. */
export interface SavedUpload {
  url: string;
  name: string;
  content_type: string;
  size: number;
}

export type SavingOutcome = { kind: "saving" } | { kind: "saved"; upload: SavedUpload } | { kind: "failed"; failure: UploadFailure };

interface Saving {
  owner: string;
  outcome: SavingOutcome;
  /** Cancels it (the transfer to Briefcase stops and its reservation is cancelled), in this copy's words. */
  cancel: () => void;
  /** Resolves once the outcome is known. */
  known: Promise<void>;
  /** Asks waiting right now, and when the last one ended (or the 202 went). */
  asking: number;
  seenAt: number;
}

/** The browser cancelled the file while Briefcase was storing it. */
export const cancelled = () => new UploadFailure(409, "upload_cancelled", "The upload was cancelled, so the file was not saved.", "Send it again if you still want it in the message.");

/** Nobody asked about the save for a while (the page was closed, or lost this website): nothing would use the file. */
export const abandoned = () =>
  new UploadFailure(409, "upload_abandoned", "This page stopped asking about the file while it was being saved (it was closed, or lost its connection), so the file was not kept.", "Send it again (it starts again from its first byte).");

/** A save this server does not know: it restarted, the outcome is older than it keeps, or it is another account's. */
export const unknownSaving = () =>
  new UploadFailure(404, "upload_unknown", "This website no longer knows that upload: it restarted while saving the file, or the upload ended a while ago.", "Send the file again (it starts again from its first byte).");

const SAVINGS = Symbol.for("dm.uploads.savings");
/** By id. On the global object, so a module loaded twice (a dev server's reload, two route bundles) shares it. */
const savings: Map<string, Saving> = ((globalThis as { [SAVINGS]?: Map<string, Saving> })[SAVINGS] ??= new Map());

/**
 * Keeps a save going after POST /api/uploads answered 202: `work` is saveToBriefcase's, `stop` aborts it (cancel,
 * abandoned). Answers the id the browser asks about. Nothing here logs the file's name.
 */
export function keepSaving(owner: string, work: Promise<SavedUpload>, stop: AbortController, timing = SAVING_TIMING): string {
  const id = randomUUID();
  const entry: Saving = { owner, outcome: { kind: "saving" }, cancel: () => stop.abort(cancelled()), asking: 0, seenAt: Date.now(), known: Promise.resolve() };
  // The tab that stopped asking: cancelled, as a transfer is when its browser goes away.
  const watch = setInterval(() => {
    if (entry.outcome.kind === "saving" && entry.asking === 0 && Date.now() - entry.seenAt > timing.abandonMs) stop.abort(abandoned());
  }, Math.max(20, Math.min(5_000, timing.abandonMs / 4)));
  watch.unref?.();
  entry.known = work.then(
    upload => {
      entry.outcome = { kind: "saved", upload };
    },
    error => {
      entry.outcome = { kind: "failed", failure: failureOf(error, "while saving") };
    },
  ).finally(() => {
    clearInterval(watch);
    setTimeout(() => {
      if (savings.get(id) === entry) savings.delete(id);
    }, timing.keepMs).unref?.();
  });
  savings.set(id, entry);
  return id;
}

/** The save `id` of `owner`, or null (unknown here, or another account's: the same answer, so ids reveal nothing). */
const savingOf = (id: string, owner: string) => {
  const entry = savings.get(id);
  return entry && entry.owner === owner ? entry : null;
};

/** Waits up to `waitMs` for the outcome of a save (the browser's ask), then says where it stands; null when unknown. */
export async function askSaving(id: string, owner: string, waitMs: number, signal?: AbortSignal): Promise<SavingOutcome | null> {
  const entry = savingOf(id, owner);
  if (!entry) return null;
  if (entry.outcome.kind === "saving" && waitMs > 0 && !signal?.aborted) {
    entry.asking += 1;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let onAbort: (() => void) | undefined;
    try {
      await Promise.race([
        entry.known,
        new Promise<void>(resolve => {
          timer = setTimeout(resolve, waitMs);
          onAbort = resolve;
          signal?.addEventListener("abort", onAbort, { once: true });
        }),
      ]);
    } finally {
      clearTimeout(timer);
      if (onAbort) signal?.removeEventListener("abort", onAbort);
      entry.asking -= 1;
      entry.seenAt = Date.now();
    }
  } else {
    entry.seenAt = Date.now();
  }
  return entry.outcome;
}

/**
 * Cancels a save (the browser's DELETE): the transfer to Briefcase stops and its reservation is cancelled. Waits up to
 * `waitMs` for that, then says where it stands ("saved" when it had already finished: the file stays in the Carbon's
 * Briefcase, with no message using it). Null when unknown.
 */
export async function cancelSaving(id: string, owner: string, waitMs = 5_000): Promise<SavingOutcome | null> {
  const entry = savingOf(id, owner);
  if (!entry) return null;
  if (entry.outcome.kind === "saving") {
    entry.cancel();
    let timer: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([entry.known, new Promise<void>(resolve => (timer = setTimeout(resolve, waitMs)))]);
    clearTimeout(timer);
  }
  return entry.outcome;
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
