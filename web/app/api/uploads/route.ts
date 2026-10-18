/**
 * POST /api/uploads?name=<file name>&size=<bytes>&sha256=<hex>: saves the request's body (the file's bytes, with its
 * real Content-Type and its exact Content-Length) into the signed-in Carbon's own Silicon Briefcase, under apps/dm/, and
 * answers the link a message carries:
 *
 *   {"type":"upload","data":{"url":"https://…/api/v1/public/<owner>/apps/dm/<name>?view=inline","name","content_type","size"}}
 *
 * Any size up to Briefcase's 5 TiB (or the operator's UPLOADS_MAX_BYTES): the bytes are piped through to Briefcase as
 * they arrive, never held here, whether on disk or in memory (lib/server/briefcase.ts). Nothing in front of this
 * handler reads or limits the body: proxy.ts (Next's proxy) does not run for /api/, so Next never copies it, and the
 * handler streams `request.body` itself. The browser hashes the file first (lib/upload/hash.ts) because Briefcase
 * reserves an upload by its size and SHA-256.
 *
 * After the last byte Briefcase stores the file before it answers, which for a large one takes minutes of silence that
 * an ingress would cut (504). So when Briefcase has not answered SAVING_TIMING.holdMs (20 s) after the last byte, this
 * answers 202 instead and the save goes on in this server:
 *
 *   {"type":"upload.saving","data":{"id":"…","name","content_type","size"}}
 *
 * and the browser asks GET /api/uploads/saving?id=… for the link (app/api/uploads/saving/route.ts), or DELETEs it.
 *
 * A server told to stop (a deploy's SIGTERM) starts no new upload (503 website_restarting: the page sends the file again
 * by itself) and stops every one under way, cancelling its reservation at Briefcase before the process exits
 * (lib/server/briefcase.ts stopUploads; prepareServer runs from instrumentation.ts).
 *
 * Errors are {"error":{code,message,hint}} in words a Carbon reads (lib/server/briefcase.ts has every one). Only this
 * site's own pages may call it (Origin and Sec-Fetch-Site), only signed in, at most MAX_UPLOADS_PER_ACCOUNT at once for
 * one account (429 too_many_uploads, Retry-After 5). A refusal decided before the bytes are read is answered at once
 * and the rest of the body is dropped for a moment, so a browser that is still sending hears it. This handler never
 * logs a file's name, its bytes, a token or a proof.
 *
 * The session is used as the request brought it and never refreshed here: an upload can outlast the refresh memo, and a
 * refresh token rotated now but written back minutes later could be presented again by the browser's session keeper
 * (which ends the whole sign-in). A token in its last 30 seconds answers 401 session_refresh_needed instead; the browser
 * renews its session and sends the file again (lib/upload/client.ts).
 */
import { after, type NextRequest, type NextResponse } from "next/server";
import { claimUploadSlot, cleanFileName, contentTypeFor, declaredFile, discardBody, failureOf, keepSaving, restarting, saveToBriefcase, SAVING_TIMING, stopping, storedName, tooManyUploads, trackUpload, UploadFailure, UploadProof, uploadsSettings, type SavedUpload } from "@/lib/server/briefcase";
import { EnvError, serverEnv } from "@/lib/server/env";
import { json, misconfigured } from "@/lib/server/responses";
import { errorBody, readSession, sameOriginProblem } from "@/lib/server/session";

export const dynamic = "force-dynamic";

const TOKEN_MARGIN_MS = 30_000;

function failed(failure: UploadFailure): NextResponse {
  return json(failure.status, failure.body(), response => {
    if (failure.retryAfter) response.headers.set("Retry-After", String(Math.ceil(failure.retryAfter)));
  });
}

/**
 * Keeps a stopping server's exit waiting for work that goes on after the answer (Next's after(), which `next start`
 * awaits before it exits). Outside a Next request (the unit tests call this handler directly) there is nothing to keep.
 */
function outlasting(work: Promise<void>): void {
  try {
    after(work);
  } catch {
    // Not in a Next request.
  }
}

export async function POST(request: NextRequest): Promise<NextResponse> {
  // Refusals before the bytes are read are answered at once; the body is dropped for a moment behind them.
  const refuse = (failure: UploadFailure) => {
    discardBody(request.body);
    return failed(failure);
  };
  let env;
  try {
    env = serverEnv();
  } catch (error) {
    if (error instanceof EnvError) {
      discardBody(request.body);
      return misconfigured(error);
    }
    throw error;
  }
  const problem = sameOriginProblem(request);
  if (problem) {
    discardBody(request.body);
    return json(403, errorBody("cross_site_request", problem, "Use this site's own pages."));
  }
  // The server is stopping (a deploy): the page sends the file again in a moment, to the server that comes next.
  if (stopping()) return refuse(restarting());
  const settings = uploadsSettings();
  if (!settings.enabled) return refuse(new UploadFailure(503, "uploads_not_configured", settings.reason, "Send a link instead (+\u00a0→\u00a0Add a link)."));
  const session = readSession(request);
  if (!session) return refuse(new UploadFailure(401, "signed_out", "This browser is not signed in (no session, or it ended).", "Sign in again."));
  if (session.ae - Date.now() < TOKEN_MARGIN_MS) {
    return refuse(new UploadFailure(401, "session_refresh_needed", "Your sign-in needs renewing before the file can be saved.", "This page renews it and sends the file again by itself; if this stays, reload the page."));
  }
  const name = cleanFileName(request.nextUrl.searchParams.get("name"));
  if (!name) return refuse(new UploadFailure(422, "invalid_name", "The upload needs the file's name: POST /api/uploads?name=<file name>&size=<bytes>&sha256=<hex>.", "Choose the file again."));
  const declared = declaredFile(request.nextUrl.searchParams, request.headers, settings);
  if (declared instanceof UploadFailure) return refuse(declared);
  if (!request.body) return refuse(new UploadFailure(422, "file_empty", "The upload carried no file.", "Choose a file and try again."));
  const contentType = contentTypeFor(request.headers.get("content-type"), name);
  const slot = claimUploadSlot(session.acct.uuid);
  if (!slot) return refuse(tooManyUploads());

  let proof: UploadProof | null = null;
  // Whether this request ends the upload (gives the slot back, revokes the proof), or a save going on after a 202 does.
  let ends = true;
  let hold: ReturnType<typeof setTimeout> | undefined;
  try {
    // First, while the access token is surely valid: the proof then lives on its own, however long the bytes take.
    proof = await UploadProof.issue(env, session.at);
    // The server began to stop while the proof was asked for: nothing is reserved yet.
    if (stopping()) throw restarting();
    const used = proof;
    // Cancelling or abandoning a save that goes on after the 202 (the request's own signal no longer aborts by then).
    const stop = new AbortController();
    let transferred: () => void = () => undefined;
    const lastByte = new Promise<void>(resolve => (transferred = resolve));
    const work: Promise<SavedUpload> = saveToBriefcase({
      settings, proof: used, name: storedName(name), contentType, file: declared, body: request.body,
      signal: AbortSignal.any([request.signal, stop.signal]), owner: session.acct.uuid, appId: env.appId, onTransferred: () => transferred(),
    }).then(({ url }) => ({ url, name, content_type: contentType, size: declared.size }));
    // Until it ends, a stopping server stops it and cancels its reservation (lib/server/briefcase.ts stopUploads).
    trackUpload(stop, work);
    // Briefcase's answer, or (every byte in, no answer after holdMs) the 202. The browser leaving meanwhile aborts the
    // save, so `work` settles at once and answers (to nobody).
    const saved = await Promise.race([
      work,
      lastByte.then(() => new Promise<null>(resolve => (hold = setTimeout(() => resolve(null), SAVING_TIMING.holdMs)))),
    ]);
    if (saved) return json(200, { type: "upload", data: saved });
    const id = keepSaving(session.acct.uuid, work, stop);
    ends = false;
    // The save goes on after this answer, holding the slot and the proof. A server told to stop waits for it (after()),
    // and stopUploads ends it at once, so its reservation is cancelled before the process exits.
    outlasting(work.then(() => undefined, () => undefined).then(() => {
      slot();
      return used.revoke();
    }));
    return json(202, { type: "upload.saving", data: { id, name, content_type: contentType, size: declared.size } });
  } catch (error) {
    const failure = failureOf(error);
    // Refused before any byte was taken (Accounts or the reservation said no): the browser may still be sending. Once
    // the bytes were being piped, the body is the transfer's (discardBody leaves a stream in use alone).
    discardBody(request.body);
    return failed(failure);
  } finally {
    clearTimeout(hold);
    if (ends) {
      slot();
      await proof?.revoke();
    }
  }
}
