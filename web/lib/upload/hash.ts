/**
 * A file's SHA-256, computed in the browser before its upload (Silicon Briefcase reserves a file by its size and
 * SHA-256, and checks the digest when the bytes arrive; the website's server only passes the bytes on, so the browser
 * is where the file can be read twice: once to hash it, once to send it).
 *
 * - Small files (up to 8 MiB: photos, voice notes) are digested by WebCrypto at once.
 * - Anything larger is hashed in a Web Worker (./hash.worker.ts) that reads it in slices, so memory stays flat whatever
 *   the size and the page stays responsive; `onProgress(done, total)` follows it ("Preparing… 40%").
 * - Where a worker cannot start, the same slices are hashed here, between frames.
 *
 * A digest is remembered per Blob for the page's life (a WeakMap: it goes with the Blob), so Retry and a resend after a
 * lost connection never read the file twice more. Cancelling (`signal`) stops the worker at once.
 */
import { Sha256, toHex } from "./sha256";

export interface HashOptions {
  signal?: AbortSignal;
  /** How much of the file is hashed (bytes), at most every 100 ms. */
  onProgress?: (done: number, total: number) => void;
}

/** Up to this size WebCrypto digests the whole file in one go (holding it in memory once is nothing at this size). */
const AT_ONCE = 8 * 1024 * 1024;
const SLICE = 8 * 1024 * 1024;

const known = new WeakMap<Blob, string>();

const aborted = () => new DOMException("The upload was cancelled.", "AbortError");

/** Why a file could not be read, in words a Carbon reads (a file moved, changed or deleted since it was chosen). */
export class UnreadableFile extends Error {
  constructor() {
    // A folder pasted or chosen where a file was expected reads the same way, so it is named too.
    super("This browser could not read the file: it is a folder, or it was moved, changed or deleted after you chose it.");
    this.name = "UnreadableFile";
  }
}

const unreadable = (error: unknown) => {
  const name = (error as { name?: unknown } | null)?.name;
  return name === "AbortError" ? error : name === "NotReadableError" || name === "NotFoundError" || name === "SecurityError" ? new UnreadableFile() : error;
};

async function atOnce(blob: Blob): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", await blob.arrayBuffer());
  return toHex(new Uint8Array(digest));
}

/** The slices hashed on this thread (no worker here): each slice is read before the next is hashed. */
async function here(blob: Blob, options: HashOptions): Promise<string> {
  const hash = new Sha256();
  for (let offset = 0; offset < blob.size; ) {
    if (options.signal?.aborted) throw aborted();
    const buffer = await blob.slice(offset, Math.min(offset + SLICE, blob.size)).arrayBuffer();
    if (!buffer.byteLength) throw new UnreadableFile();
    hash.update(new Uint8Array(buffer));
    offset += buffer.byteLength;
    options.onProgress?.(offset, blob.size);
  }
  return hash.hex();
}

function startWorker(): Worker | null {
  if (typeof Worker === "undefined") return null;
  try {
    return new Worker(new URL("./hash.worker.ts", import.meta.url), { type: "module", name: "dm-upload-hash" });
  } catch {
    return null;
  }
}

function inWorker(worker: Worker, blob: Blob, options: HashOptions): Promise<string> {
  return new Promise((resolve, reject) => {
    const stop = () => {
      worker.terminate();
      reject(aborted());
    };
    const done = () => {
      worker.terminate();
      options.signal?.removeEventListener("abort", stop);
    };
    options.signal?.addEventListener("abort", stop, { once: true });
    worker.onmessage = (event: MessageEvent<{ type: string; done?: number; sha256?: string; size?: number; name?: string; message?: string }>) => {
      const message = event.data;
      if (message.type === "progress") options.onProgress?.(message.done ?? 0, blob.size);
      else if (message.type === "done" && message.sha256 && message.size === blob.size) {
        done();
        resolve(message.sha256);
      } else {
        done();
        reject(message.type === "done" ? new UnreadableFile() : unreadable(Object.assign(new Error(message.message ?? "The file could not be read."), { name: message.name ?? "Error" })));
      }
    };
    // A worker that cannot run its script (a policy, a missing chunk) says so here: the file is hashed on this thread.
    worker.onerror = event => {
      event.preventDefault();
      done();
      here(blob, options).then(resolve, reject);
    };
    worker.postMessage({ blob });
  });
}

/** The file's SHA-256 as 64 lowercase hex characters. */
export async function sha256Of(blob: Blob, options: HashOptions = {}): Promise<string> {
  if (options.signal?.aborted) throw aborted();
  const remembered = known.get(blob);
  if (remembered) {
    options.onProgress?.(blob.size, blob.size);
    return remembered;
  }
  let digest: string;
  try {
    if (blob.size <= AT_ONCE && typeof crypto !== "undefined" && crypto.subtle) digest = await atOnce(blob);
    else {
      const worker = startWorker();
      digest = worker ? await inWorker(worker, blob, options) : await here(blob, options);
    }
  } catch (error) {
    throw unreadable(error);
  }
  if (options.signal?.aborted) throw aborted();
  known.set(blob, digest);
  options.onProgress?.(blob.size, blob.size);
  return digest;
}
