/**
 * The Web Worker that prepares a file for upload: reads it in 8 MiB slices (the next slice is read while this one is
 * hashed) and feeds them to ./sha256.ts, so a file of any size is hashed with a few slices in memory and the page stays
 * responsive. ./hash.ts starts one per file and ends it when the file is done or the upload is cancelled.
 *
 *   in:  {blob}
 *   out: {type: "progress", done} at most every 100 ms, then {type: "done", sha256, size} or {type: "error", name, message}
 */
import { Sha256 } from "./sha256";

const SLICE = 8 * 1024 * 1024;
const REPORT_MS = 100;

/* The worker's own scope, typed by what this file uses (the project's types are the page's, not a worker's). */
const scope = self as unknown as { onmessage: ((event: MessageEvent<{ blob: Blob }>) => void) | null; postMessage: (message: unknown) => void };

async function prepare(blob: Blob): Promise<void> {
  const hash = new Sha256();
  let offset = 0;
  let reported = 0;
  let next: Promise<ArrayBuffer> | null = blob.size > 0 ? blob.slice(0, Math.min(SLICE, blob.size)).arrayBuffer() : null;
  while (next) {
    const buffer = await next;
    if (!buffer.byteLength) throw new DOMException("The file ended before its size.", "NotReadableError");
    const end = offset + buffer.byteLength;
    // Reading the next slice runs while this one is hashed.
    next = end < blob.size ? blob.slice(end, Math.min(end + SLICE, blob.size)).arrayBuffer() : null;
    hash.update(new Uint8Array(buffer));
    offset = end;
    const now = performance.now();
    if (now - reported >= REPORT_MS) {
      reported = now;
      scope.postMessage({ type: "progress", done: offset });
    }
  }
  scope.postMessage({ type: "done", sha256: hash.hex(), size: offset });
}

scope.onmessage = event => {
  prepare(event.data.blob).catch((error: unknown) => {
    const failure = error as { name?: unknown; message?: unknown } | null;
    scope.postMessage({ type: "error", name: typeof failure?.name === "string" ? failure.name : "Error", message: typeof failure?.message === "string" ? failure.message : String(error) });
  });
};
