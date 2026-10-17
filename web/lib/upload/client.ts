/**
 * Uploading a file or a voice note into the signed-in Carbon's own Silicon Briefcase, through this website's server
 * (`POST /api/uploads`, spec §6.9). DM's service never handles files: a message carries only the link this returns.
 *
 * Shared by the composer (files, paste, drop) and the voice area (voice notes). The composer area owns this module and
 * the server side (app/api/uploads/**, lib/server/briefcase.ts); the voice area imports it. XMLHttpRequest rather than
 * fetch because only it reports upload progress.
 *
 * The session is renewed first when it is about to run out (the server never renews it during an upload: see
 * app/api/uploads/route.ts), and a `session_refresh_needed` answer renews it and sends the file once more.
 *
 * Offline, an upload waits for the connection instead of failing (what the website promises for everything you send:
 * it waits and goes out when you are back). A connection lost on the way stops the transfer at once (the browser's
 * `offline`), waits, and sends the file again from the start, with `onProgress(0, size)` so the tray shows it; a
 * failure while the browser still believes it is online is reported as it is (the Carbon can retry it).
 */
import { ApiError } from "@/lib/errors";
import { ensureFreshSession, markSignedOut, refreshSessionNow } from "@/lib/client/session";
import { rememberUploaded } from "./display";

/** What `POST /api/uploads` answers: the link a message carries. */
export interface UploadedFile {
  url: string;
  name: string;
  content_type: string;
  size: number;
}

/** `GET /api/uploads/config`: whether uploads work here, and why not when they don't. */
export interface UploadsConfig {
  enabled: boolean;
  reason?: string;
  max_bytes: number;
}

export interface UploadOptions {
  /** The file name to keep (a voice note gets one such as `voice-note-2026-10-17-1532.webm`). */
  name: string;
  /** Defaults to the blob's own type. */
  contentType?: string;
  signal?: AbortSignal;
  onProgress?: (sent: number, total: number) => void;
}

const UPLOADS = "/api/uploads";
/** How many times a transfer cut by a lost connection starts again once the connection is back. */
const RESENDS_AFTER_OFFLINE = 5;

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
  return { enabled: config.enabled, ...(config.reason ? { reason: config.reason } : {}), max_bytes: Number(config.max_bytes) || 0 };
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

const offlineError = () => new ApiError({ status: 0, code: "network_error", message: "The connection was lost while the file was uploading.", hint: "It uploads again when you are back online.", method: "POST", path: UPLOADS });

/** One attempt: the bytes in the body, the name in the query, progress from XMLHttpRequest. */
function send(file: Blob, options: UploadOptions, contentType: string): Promise<UploadedFile> {
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
    xhr.open("POST", `${UPLOADS}?${new URLSearchParams({ name: options.name })}`);
    xhr.setRequestHeader("content-type", contentType);
    xhr.responseType = "json";
    xhr.upload.onprogress = event => options.onProgress?.(event.loaded, event.lengthComputable ? event.total : file.size);
    xhr.onload = () => {
      done();
      const body = xhr.response as { type?: string; data?: UploadedFile; error?: { code?: string; message?: string; hint?: string; details?: Record<string, unknown> } } | null;
      if (xhr.status >= 200 && xhr.status < 300 && body?.data?.url) return resolve(body.data);
      const retryAfter = Number(xhr.getResponseHeader("retry-after"));
      reject(
        new ApiError({
          status: xhr.status,
          code: body?.error?.code ?? "upload_failed",
          // An answer without the website's own words (a proxy in between, a crash): what happened, in plain words.
          message: body?.error?.message ?? (xhr.status >= 500 ? "The upload failed: this website could not finish it." : "The upload was refused, and the website did not say why."),
          hint: body?.error?.hint ?? (xhr.status >= 500 ? "Try again in a moment." : "Try again; if it keeps failing, report it in Settings → Report a problem."),
          details: body?.error?.details,
          retryAfter: Number.isFinite(retryAfter) && retryAfter > 0 ? retryAfter : null,
          method: "POST",
          path: UPLOADS,
        }),
      );
    };
    xhr.onerror = () => {
      done();
      reject(wentOffline || !browserOnline() ? offlineError() : new ApiError({ status: 0, code: "network_error", message: "The upload could not reach this website.", hint: "Check the connection, then try again.", method: "POST", path: UPLOADS }));
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
async function sendSignedIn(file: Blob, options: UploadOptions, contentType: string): Promise<UploadedFile> {
  await ensureFreshSession();
  try {
    return await send(file, options, contentType);
  } catch (error) {
    if (!ApiError.is(error, "session_refresh_needed", "signed_out")) throw error;
    if (error.code === "signed_out") {
      markSignedOut();
      throw error;
    }
    await refreshSessionNow();
    return send(file, options, contentType);
  }
}

export async function uploadFile(file: Blob, options: UploadOptions): Promise<UploadedFile> {
  const contentType = options.contentType || file.type || "application/octet-stream";
  let uploaded: UploadedFile | null = null;
  for (let attempt = 0; !uploaded; attempt++) {
    await whenOnline(options.signal);
    // Starting again from the first byte: the tray shows it.
    if (attempt > 0) options.onProgress?.(0, file.size);
    try {
      uploaded = await sendSignedIn(file, options, contentType);
    } catch (error) {
      // The connection went away on the way: wait for it, then send the file again.
      if (ApiError.is(error, "network_error") && !browserOnline() && attempt < RESENDS_AFTER_OFFLINE) continue;
      throw error;
    }
  }
  // Your own file shows from the bytes this tab has (lib/upload/display.ts), not from a second download.
  rememberUploaded(uploaded.url, file);
  return uploaded;
}
