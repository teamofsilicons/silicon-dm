/**
 * Uploading a file or a voice note into the signed-in Carbon's own Silicon Briefcase, through this website's server
 * (`POST /api/uploads`, spec §6.9). DM's service never handles files: a message carries only the link this returns.
 *
 * Shared by the composer (the + menu, /upload, ⌘⇧U, paste, drag and drop) and the voice area (voice notes). The
 * composer area owns this module and the server side (app/api/uploads/**, lib/server/briefcase.ts); the voice area
 * imports it. Any size, up to Silicon Briefcase's 5 TiB per file (or the website operator's limit, GET /api/uploads/config):
 *
 *   1. preparing: the file's SHA-256, read in slices in a Web Worker (./hash.ts), with `onPrepare(done, total)`;
 *   2. sending: `POST /api/uploads?name=…&size=…&sha256=…` with the File itself as the body (XMLHttpRequest streams it
 *      from disk with its exact Content-Length, and is the only API that reports upload progress), `onProgress(sent,
 *      total)`; the website's server pipes it into Briefcase as it arrives;
 *   3. saving: every byte sent, the server commits the file and opens its link (`onProgress(total, total)` says so).
 *      Briefcase stores a large file for minutes before it answers, and an ingress would cut a request that silent, so
 *      the server may answer 202 instead (the save goes on there): this then asks `GET /api/uploads/saving?id=…`, each
 *      ask waiting up to 25 s, until it has the link. Cancelling then DELETEs it, and so does leaving the page.
 *
 * The session is renewed first when it is about to run out (the server never renews it during an upload: see
 * app/api/uploads/route.ts), and a `session_refresh_needed` answer renews it and sends the file once more.
 *
 * Offline, an upload waits for the connection instead of failing (what the website promises for everything you send:
 * it waits and goes out when you are back). A connection lost on the way stops the transfer at once (the browser's
 * `offline`), waits, and sends the file again from its first byte (Briefcase takes a file in one piece; its digest is
 * remembered, so it is not prepared twice), with `onRestart()` and `onProgress(0, size)` so the tray can say so. A
 * website that restarts while it has the file (a deploy: it answers `website_restarting`, a save it had is unknown to
 * the next one, or a gateway answers for it meanwhile) gets the file again the same way, after the wait it asks for.
 * Any other failure while the browser still believes it is online is reported as it is (the Carbon can retry it). Cancelling
 * (`signal`) stops the preparation or the transfer at once; the website's server then cancels Briefcase's reservation.
 */
import { ApiError } from "@/lib/errors";
import { ensureFreshSession, markSignedOut, refreshSessionNow } from "@/lib/client/session";
import { rememberUploaded } from "./display";
import { sha256Of, UnreadableFile } from "./hash";

/** What `POST /api/uploads` answers: the link a message carries. */
export interface UploadedFile {
  url: string;
  name: string;
  content_type: string;
  size: number;
}

/** `GET /api/uploads/config`: whether uploads work here, why not when they don't, and the largest file taken. */
export interface UploadsConfig {
  enabled: boolean;
  reason?: string;
  /** The largest file: Silicon Briefcase's 5 TiB, or the website operator's lower limit; 0 when not known. */
  max_bytes: number;
  /** Whether `max_bytes` is the website operator's own limit (UPLOADS_MAX_BYTES) rather than Briefcase's. */
  capped?: boolean;
}

/** The largest file Silicon Briefcase takes (5 TiB). */
export const BRIEFCASE_MAX_BYTES = 5 * 1024 ** 4;

/**
 * What Silicon Briefcase takes into one Briefcase in a UTC day unless its operators gave that one more (100 GiB; the
 * website's lib/server/briefcase.ts BRIEFCASE_DAILY_UPLOAD_BYTES). A larger file is refused once it is prepared, on any
 * day, so the composer says so when one is added; this website cannot see a Briefcase's own allowance, so it never
 * refuses one for it.
 */
export const BRIEFCASE_DAILY_BYTES = 100 * 1024 ** 3;

export interface UploadOptions {
  /** The file name to keep (a voice note gets one such as `voice-note-2026-10-17-1532.webm`). */
  name: string;
  /** Defaults to the blob's own type. */
  contentType?: string;
  signal?: AbortSignal;
  /** Bytes sent of the whole; `(size, size)` once every byte is sent and the website is saving the file. */
  onProgress?: (sent: number, total: number) => void;
  /** Bytes of the file prepared (hashed) before it is sent, for a file big enough that this takes a moment. */
  onPrepare?: (done: number, total: number) => void;
  /** The transfer starts again from its first byte (the connection was lost on the way). */
  onRestart?: () => void;
  /** Every byte arrived and Silicon Briefcase is storing a large file, which takes a while (the website answered 202). */
  onSaving?: () => void;
}

const UPLOADS = "/api/uploads";
const SAVING = `${UPLOADS}/saving`;
/** How many times a transfer cut by a lost connection (or a website restarting) starts again. */
const RESENDS_AFTER_OFFLINE = 5;
/** The longest wait before sending a file again to a website that is restarting. */
const RESEND_WAIT_MAX_MS = 15_000;
/** How long one ask about a file being saved waits on the server (it answers sooner when the file is saved). */
const SAVING_WAIT_SECONDS = 25;
/** Asks about a save that may fail on the way (the connection blinked) before the upload fails. */
const SAVING_ASK_FAILURES = 6;

/**
 * Whether this website uploads. When the answer cannot be had (offline, a blip), uploads are assumed to work with no
 * size known (`max_bytes` 0): the upload itself then says exactly why, if it fails, instead of a guess switching them off.
 */
export async function getUploadsConfig(signal?: AbortSignal): Promise<UploadsConfig> {
  const unknown: UploadsConfig = { enabled: true, max_bytes: 0 };
  let response: Response;
  try {
    response = await fetch(`${UPLOADS}/config`, { signal, credentials: "same-origin", cache: "no-store" });
  } catch (error) {
    if (error && typeof error === "object" && (error as { name?: unknown }).name === "AbortError") throw error;
    return unknown;
  }
  if (!response.ok) return unknown;
  const body = (await response.json().catch(() => null)) as (UploadsConfig & { data?: UploadsConfig }) | null;
  const config = body?.data ?? body;
  if (!config || typeof config.enabled !== "boolean") return unknown;
  return { enabled: config.enabled, ...(config.reason ? { reason: config.reason } : {}), max_bytes: Number(config.max_bytes) || 0, capped: config.capped === true };
}

const aborted = () => new DOMException("The upload was cancelled.", "AbortError");

/** Whether the browser believes it has a connection (true where it cannot tell). */
export const browserOnline = () => typeof navigator === "undefined" || navigator.onLine !== false;

/** Resolves at once while the browser is online, otherwise when it is back; rejects when `signal` aborts first. */
export function whenOnline(signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.reject(aborted());
  if (browserOnline() || typeof window === "undefined") return Promise.resolve();
  return new Promise((resolve, reject) => {
    const back = () => {
      window.removeEventListener("online", back);
      signal?.removeEventListener("abort", stop);
      resolve();
    };
    const stop = () => {
      window.removeEventListener("online", back);
      reject(aborted());
    };
    window.addEventListener("online", back);
    signal?.addEventListener("abort", stop, { once: true });
  });
}

const offlineError = () => new ApiError({ status: 0, code: "network_error", message: "The connection was lost while the file was uploading.", hint: "It uploads again from its first byte when you are back online.", method: "POST", path: UPLOADS });

/** The address of one upload: the file's name, its size and its SHA-256 (Briefcase reserves by both before the bytes). */
export const uploadAddress = (name: string, size: number, sha256: string) => `${UPLOADS}?${new URLSearchParams({ name, size: String(size), sha256 })}`;

/** Every byte went, and the website is still saving the file into Briefcase (202): ask about it by its id. */
interface StillSaving {
  saving: string;
}

type ErrorBody = { error?: { code?: string; message?: string; hint?: string; details?: Record<string, unknown> } } | null;

/** A refusal from the website, in its own words; an answer without them (a proxy in between, a crash) in plain ones. */
function refusal(status: number, body: ErrorBody, retryAfterHeader: string | null, method: string, path: string): ApiError {
  const retryAfter = Number(retryAfterHeader);
  if (!body?.error && status >= 502 && status <= 504) {
    // A gateway in front answered for the website: it is restarting (a deploy), or a connection was cut on the way in.
    // Nothing reached Briefcase from this request that a new one would not replace, so the file goes again by itself.
    return new ApiError({ status, code: "network_error", message: `This website could not be reached (something in between answered HTTP ${status}).`, hint: "Try again in a moment (the file starts again from its first byte); if it keeps failing, check the connection.", retryAfter: Number.isFinite(retryAfter) && retryAfter > 0 ? retryAfter : null, method, path });
  }
  return new ApiError({
    status,
    code: body?.error?.code ?? "upload_failed",
    message: body?.error?.message ?? (status === 413 ? "The upload was refused as too large before it reached this website (by a proxy in between)." : status >= 500 ? "The upload failed: this website could not finish it." : "The upload was refused, and the website did not say why."),
    hint: body?.error?.hint ?? (status === 413 ? "Share a link to the file instead (+\u00a0→\u00a0Add a link); this website's maintainers can lift the proxy's body limit." : status >= 500 ? "Try again in a moment." : "Try again; if it keeps failing, report it in Settings → Report a problem."),
    details: body?.error?.details,
    retryAfter: Number.isFinite(retryAfter) && retryAfter > 0 ? retryAfter : null,
    method,
    path,
  });
}

/** One attempt: the bytes in the body, the name, size and digest in the query, progress from XMLHttpRequest. */
function send(file: Blob, options: UploadOptions, contentType: string, sha256: string): Promise<UploadedFile | StillSaving> {
  return new Promise((resolve, reject) => {
    if (options.signal?.aborted) return reject(aborted());
    const xhr = new XMLHttpRequest();
    let wentOffline = false;
    // A lost connection can leave a transfer hanging for minutes: stop it the moment the browser says so.
    const onOffline = () => {
      wentOffline = true;
      xhr.abort();
    };
    const onAbort = () => xhr.abort();
    const done = () => {
      window.removeEventListener("offline", onOffline);
      options.signal?.removeEventListener("abort", onAbort);
    };
    xhr.open("POST", uploadAddress(options.name, file.size, sha256));
    xhr.setRequestHeader("content-type", contentType);
    xhr.responseType = "json";
    xhr.upload.onprogress = event => options.onProgress?.(event.loaded, event.lengthComputable ? event.total : file.size);
    xhr.upload.onload = () => options.onProgress?.(file.size, file.size);
    xhr.onload = () => {
      done();
      const body = xhr.response as ({ type?: string; data?: UploadedFile & { id?: string } } & ErrorBody) | null;
      if (xhr.status === 202 && body?.type === "upload.saving" && typeof body.data?.id === "string") return resolve({ saving: body.data.id });
      if (xhr.status >= 200 && xhr.status < 300 && body?.data?.url) return resolve(body.data);
      reject(refusal(xhr.status, body, xhr.getResponseHeader("retry-after"), "POST", UPLOADS));
    };
    xhr.onerror = () => {
      done();
      reject(wentOffline || !browserOnline() ? offlineError() : new ApiError({ status: 0, code: "network_error", message: "The upload could not reach this website, or the connection broke on the way.", hint: "Check the connection, then try again (the file starts again from its first byte).", method: "POST", path: UPLOADS }));
    };
    xhr.onabort = () => {
      done();
      reject(wentOffline ? offlineError() : aborted());
    };
    window.addEventListener("offline", onOffline);
    options.signal?.addEventListener("abort", onAbort, { once: true });
    xhr.send(file);
  });
}

/** One upload with the session renewed first, and once more after `session_refresh_needed`. */
async function sendSignedIn(file: Blob, options: UploadOptions, contentType: string, sha256: string): Promise<UploadedFile | StillSaving> {
  await ensureFreshSession();
  try {
    return await send(file, options, contentType, sha256);
  } catch (error) {
    if (!ApiError.is(error, "session_refresh_needed", "signed_out")) throw error;
    if (error.code === "signed_out") {
      markSignedOut();
      throw error;
    }
    await refreshSessionNow();
    return send(file, options, contentType, sha256);
  }
}

/** The file could not be read (moved, changed or deleted since it was chosen), as an ApiError the tray shows. */
const unreadable = () => new ApiError({ status: 0, code: "file_unreadable", message: new UnreadableFile().message, hint: "Choose the file again; to send a folder's files, drop the folder on the conversation.", method: "POST", path: UPLOADS });

const isAbort = (error: unknown) => !!error && typeof error === "object" && (error as { name?: unknown }).name === "AbortError";

const pause = (ms: number, signal?: AbortSignal) =>
  new Promise<void>((resolve, reject) => {
    if (signal?.aborted) return reject(aborted());
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", stop);
      resolve();
    }, ms);
    const stop = () => {
      clearTimeout(timer);
      reject(aborted());
    };
    signal?.addEventListener("abort", stop, { once: true });
  });

/** Saves this tab is waiting for: cancelled at the website when the page goes (nothing would use the file). */
const savingHere = new Set<string>();
let leavingWatched = false;

/** Cancels a save at the website, even from a page that is going away (keepalive). Best effort: an unanswered save is cancelled anyway. */
function cancelSave(id: string) {
  try {
    void fetch(`${SAVING}?${new URLSearchParams({ id })}`, { method: "DELETE", keepalive: true, credentials: "same-origin", cache: "no-store" }).catch(() => undefined);
  } catch {
    // keepalive refused (too much in flight): the website cancels a save nobody asks about by itself.
  }
}

function watchLeaving() {
  if (leavingWatched || typeof window === "undefined") return;
  leavingWatched = true;
  window.addEventListener("pagehide", event => {
    // A page kept for the back button comes back to its uploads; one that goes, cancels them.
    if (event.persisted) return;
    for (const id of savingHere) cancelSave(id);
  });
}

/**
 * Every byte was sent and the website saves the file into Briefcase on its own (POST answered 202): asks
 * `GET /api/uploads/saving` until it says how it ended. The website keeps saving whatever happens to this tab's
 * connection meanwhile, so a blink is waited out (offline: until the browser is back); cancelling DELETEs the save.
 */
async function untilSaved(id: string, signal?: AbortSignal): Promise<UploadedFile> {
  savingHere.add(id);
  watchLeaving();
  try {
    for (let failures = 0; ; ) {
      await whenOnline(signal);
      const asked = Date.now();
      let response: Response;
      try {
        response = await fetch(`${SAVING}?${new URLSearchParams({ id, wait: String(SAVING_WAIT_SECONDS) })}`, { signal, credentials: "same-origin", cache: "no-store" });
      } catch (error) {
        if (isAbort(error)) throw error;
        if (!browserOnline()) continue;
        failures += 1;
        if (failures > SAVING_ASK_FAILURES) throw new ApiError({ status: 0, code: "network_error", message: "This page lost touch with the website while it saved the file.", hint: "Check the connection, then send the file again (it starts again from its first byte).", method: "GET", path: SAVING });
        await pause(Math.min(8_000, 500 * 2 ** failures), signal);
        continue;
      }
      const body = (await response.json().catch(() => null)) as ({ type?: string; data?: UploadedFile } & ErrorBody) | null;
      // A gateway's answer without the website's words (it is restarting, or briefly unreachable): ask again, as for a
      // blink. A website that restarted then answers upload_unknown, and the file goes again.
      if (response.status >= 500 && !body?.error) {
        failures += 1;
        if (failures > SAVING_ASK_FAILURES) throw refusal(response.status, body, response.headers.get("retry-after"), "GET", SAVING);
        await pause(Math.min(8_000, 500 * 2 ** failures), signal);
        continue;
      }
      failures = 0;
      if (response.status === 202) {
        // An ask answered at once (it should wait): never faster than once a second.
        if (Date.now() - asked < 1_000) await pause(1_000, signal);
        continue;
      }
      if (response.ok && body?.data?.url) return body.data;
      throw refusal(response.status, body, response.headers.get("retry-after"), "GET", SAVING);
    }
  } catch (error) {
    if (isAbort(error) || signal?.aborted) cancelSave(id);
    throw error;
  } finally {
    savingHere.delete(id);
  }
}

/** A gateway answered for the website instead of the website itself (refusal above). */
const gatewayAnswered = (error: unknown) => ApiError.is(error, "network_error") && error.status >= 502 && error.status <= 504;

/**
 * The bytes were lost on the way to Briefcase, or the website restarted (a deploy) and let the file go, so it goes again
 * from its first byte (the tray says so).
 */
const sendAgain = (error: unknown) => (ApiError.is(error, "network_error") && !browserOnline()) || gatewayAnswered(error) || ApiError.is(error, "upload_abandoned", "upload_unknown", "website_restarting");

/**
 * How long to wait before sending again: a website that is restarting is given the time it asked for (Retry-After),
 * else a little longer each time; anything else goes at once (offline, it waits for the connection first).
 */
function resendWait(error: unknown, attempt: number): number {
  if (!ApiError.is(error, "website_restarting") && !gatewayAnswered(error)) return 0;
  const asked = ApiError.from(error).retryAfter;
  return Math.min(RESEND_WAIT_MAX_MS, asked ? asked * 1000 : 2_000 * 2 ** attempt);
}

export async function uploadFile(file: Blob, options: UploadOptions): Promise<UploadedFile> {
  const contentType = options.contentType || file.type || "application/octet-stream";
  let sha256: string;
  try {
    sha256 = await sha256Of(file, { signal: options.signal, onProgress: options.onPrepare });
  } catch (error) {
    if (error instanceof UnreadableFile) throw unreadable();
    throw error;
  }
  let uploaded: UploadedFile | null = null;
  for (let attempt = 0; !uploaded; attempt++) {
    await whenOnline(options.signal);
    // Starting again from the first byte: the tray shows it.
    if (attempt > 0) {
      options.onRestart?.();
      options.onProgress?.(0, file.size);
    }
    try {
      const sent = await sendSignedIn(file, options, contentType, sha256);
      if ("saving" in sent) options.onSaving?.();
      uploaded = "saving" in sent ? await untilSaved(sent.saving, options.signal) : sent;
    } catch (error) {
      // The connection went away on the way (or the website lost the save it was making, or restarted): wait for the
      // connection (and a restarting website), then send the file again.
      if (sendAgain(error) && attempt < RESENDS_AFTER_OFFLINE) {
        const wait = resendWait(error, attempt);
        if (wait) await pause(wait, options.signal);
        continue;
      }
      throw error;
    }
  }
  // Your own file shows from the bytes this tab has (lib/upload/display.ts), not from a second download.
  rememberUploaded(uploaded.url, file);
  return uploaded;
}
