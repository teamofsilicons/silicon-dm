/**
 * Uploading a file or a voice note into the signed-in Carbon's own Silicon Briefcase, through this website's server
 * (`POST /api/uploads`, spec §6.9). DM's service never handles files: a message carries only the link this returns.
 *
 * Shared by the composer (files, paste, drop) and the voice area (voice notes). The composer area owns this module and
 * the server side (app/api/uploads/**, lib/server/briefcase.ts); the voice area imports it. XMLHttpRequest rather than
 * fetch because only it reports upload progress.
 */
import { ApiError } from "@/lib/errors";

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

export async function getUploadsConfig(signal?: AbortSignal): Promise<UploadsConfig> {
  const response = await fetch(`${UPLOADS}/config`, { signal, credentials: "same-origin", cache: "no-store" });
  if (!response.ok) return { enabled: false, reason: "Uploads could not be checked right now.", max_bytes: 0 };
  return (await response.json()) as UploadsConfig;
}

export function uploadFile(file: Blob, options: UploadOptions): Promise<UploadedFile> {
  return new Promise((resolve, reject) => {
    const contentType = options.contentType || file.type || "application/octet-stream";
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `${UPLOADS}?${new URLSearchParams({ name: options.name })}`);
    xhr.setRequestHeader("content-type", contentType);
    xhr.responseType = "json";
    xhr.upload.onprogress = event => options.onProgress?.(event.loaded, event.lengthComputable ? event.total : file.size);
    xhr.onload = () => {
      const body = xhr.response as { type?: string; data?: UploadedFile; error?: { code?: string; message?: string; hint?: string; details?: Record<string, unknown> } } | null;
      if (xhr.status >= 200 && xhr.status < 300 && body?.data?.url) return resolve(body.data);
      reject(
        new ApiError({
          status: xhr.status,
          code: body?.error?.code ?? "upload_failed",
          message: body?.error?.message ?? `The upload failed (HTTP ${xhr.status}).`,
          hint: body?.error?.hint,
          details: body?.error?.details,
          method: "POST",
          path: UPLOADS,
        }),
      );
    };
    xhr.onerror = () =>
      reject(new ApiError({ status: 0, code: "network_error", message: "The upload could not reach this website.", hint: "Check the connection, then try again.", method: "POST", path: UPLOADS }));
    xhr.onabort = () => reject(new DOMException("The upload was cancelled.", "AbortError"));
    if (options.signal?.aborted) return reject(new DOMException("The upload was cancelled.", "AbortError"));
    options.signal?.addEventListener("abort", () => xhr.abort(), { once: true });
    xhr.send(file);
  });
}
