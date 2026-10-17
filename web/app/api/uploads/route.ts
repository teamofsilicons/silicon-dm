/**
 * POST /api/uploads?name=<file name>: saves the request's body (the file's bytes, with its real Content-Type) into the
 * signed-in Carbon's own Silicon Briefcase, under apps/dm/, and answers the link a message carries:
 *
 *   {"type":"upload","data":{"url":"https://…/api/v1/public/<owner>/apps/dm/<name>?view=inline","name","content_type","size"}}
 *
 * Errors are {"error":{code,message,hint}} in words a Carbon reads (lib/server/briefcase.ts has every one). Only this
 * site's own pages may call it (Origin and Sec-Fetch-Site), only signed in, at most UPLOADS_MAX_BYTES, at most
 * MAX_UPLOADS_PER_ACCOUNT at once for one account (429 too_many_uploads, Retry-After 5). The flow, the
 * proofs and the cleanup are in lib/server/briefcase.ts; this handler puts them in order and never logs a file's name,
 * its bytes, a token or a proof.
 *
 * The session is used as the request brought it and never refreshed here: an upload can outlast the refresh memo, and a
 * refresh token rotated now but written back minutes later could be presented again by the browser's session keeper
 * (which ends the whole sign-in). A token in its last 30 seconds answers 401 session_refresh_needed instead; the browser
 * renews its session and sends the file again (lib/upload/client.ts).
 */
import type { NextRequest, NextResponse } from "next/server";
import { rm } from "node:fs/promises";
import { claimUploadSlot, contentTypeFor, cleanFileName, drain, publishToBriefcase, receiveFile, storedName, tooLarge, tooManyUploads, UploadFailure, UploadProof, uploadsSettings, type ReceivedFile } from "@/lib/server/briefcase";
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

export async function POST(request: NextRequest): Promise<NextResponse> {
  let env;
  try {
    env = serverEnv();
  } catch (error) {
    if (error instanceof EnvError) return misconfigured(error);
    throw error;
  }
  const problem = sameOriginProblem(request);
  if (problem) return json(403, errorBody("cross_site_request", problem, "Use this site's own pages."));
  const settings = uploadsSettings();
  // Refusals before the bytes are read still read them (up to the cap), so a browser that is sending hears the answer.
  const refuse = async (failure: UploadFailure) => {
    await drain(request.body, settings.maxBytes);
    return failed(failure);
  };
  if (!settings.enabled) return refuse(new UploadFailure(503, "uploads_not_configured", settings.reason, "Send a link instead (+ → Add a link)."));
  const session = readSession(request);
  if (!session) return refuse(new UploadFailure(401, "signed_out", "This browser is not signed in (no session, or it ended).", "Sign in again."));
  if (session.ae - Date.now() < TOKEN_MARGIN_MS) {
    return refuse(new UploadFailure(401, "session_refresh_needed", "Your sign-in needs renewing before the file can be saved.", "This page renews it and sends the file again by itself; if this stays, reload the page."));
  }
  const name = cleanFileName(request.nextUrl.searchParams.get("name"));
  if (!name) return refuse(new UploadFailure(422, "invalid_name", "The upload needs the file's name: POST /api/uploads?name=<file name>.", "Choose the file again."));
  const declared = Number(request.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > settings.maxBytes) return refuse(tooLarge(settings.maxBytes));
  const contentType = contentTypeFor(request.headers.get("content-type"), name);
  const slot = claimUploadSlot(session.acct.uuid);
  if (!slot) return refuse(tooManyUploads());

  let proof: UploadProof | null = null;
  let file: ReceivedFile | null = null;
  try {
    // First, while the access token is surely valid: the proof then lives on its own, however long the bytes take.
    proof = await UploadProof.issue(env, session.at);
    file = await receiveFile(request.body, settings.maxBytes, request.signal);
    const { url } = await publishToBriefcase({ settings, proof, file, name: storedName(name), contentType, owner: session.acct.uuid, appId: env.appId });
    return json(200, { type: "upload", data: { url, name, content_type: contentType, size: file.size } });
  } catch (error) {
    const failure = UploadFailure.from(error);
    if (!(error instanceof UploadFailure)) console.error(`upload failed: ${error instanceof Error ? `${error.name}: ${error.message}` : String(error)}`);
    if (!file) await drain(request.body, settings.maxBytes).catch(() => undefined);
    return failed(failure);
  } finally {
    slot();
    await proof?.revoke();
    if (file) await rm(file.path, { force: true }).catch(() => undefined);
  }
}
