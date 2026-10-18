/**
 * GET /api/uploads/saving?id=<id>[&wait=<seconds>]: a file this website is still saving into Silicon Briefcase after its
 * last byte (POST /api/uploads answered 202, because Briefcase stores a large file for minutes before it answers and an
 * ingress cuts a request that silent). Each ask waits up to `wait` seconds (0 to 25, default 25) for the outcome:
 *
 *   200 {"type":"upload","data":{"url","name","content_type","size"}}       saved: the link a message carries
 *   202 {"type":"upload.saving","data":{"id"}}                               still saving: ask again
 *   4xx/5xx {"error":{code,message,hint}}                                     not saved, in the upload's own words
 *   404 upload_unknown                                                        this server does not know it
 *
 * DELETE /api/uploads/saving?id=<id>: cancels it (the transfer to Briefcase stops and its reservation is cancelled):
 * 200 {"type":"upload.cancel","data":{"id","state":…}} with the state as it is:
 *   "cancelled"    stopped, and Briefcase let go of the reservation
 *   "stopped"      stopped here, but Briefcase did not let go (it refused the cancel, or did not answer): the unfinished
 *                  upload stays reserved there until it expires, within a day (the server's log says why)
 *   "cancelling"   still stopping after a few seconds
 *   "saved"        it had already finished: the file stays in the Carbon's Briefcase, in no message
 *
 * Only the Carbon whose upload it is may ask (another account's id is unknown, like one that never was), and a DELETE
 * only from this site's own pages. A save nobody asks about for a while is cancelled (lib/server/briefcase.ts
 * SAVING_TIMING), so the page keeps asking while it waits.
 */
import type { NextRequest, NextResponse } from "next/server";
import { askSaving, cancelSaving, SAVING_TIMING, unknownSaving, type SavingOutcome, type UploadFailure } from "@/lib/server/briefcase";
import { json } from "@/lib/server/responses";
import { errorBody, readSession, sameOriginProblem } from "@/lib/server/session";

export const dynamic = "force-dynamic";

const ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function failed(failure: UploadFailure): NextResponse {
  return json(failure.status, failure.body(), response => {
    if (failure.retryAfter) response.headers.set("Retry-After", String(Math.ceil(failure.retryAfter)));
  });
}

/** The signed-in account and the save it names, or the answer that refuses. */
function caller(request: NextRequest): { owner: string; id: string } | NextResponse {
  const session = readSession(request);
  if (!session) return json(401, errorBody("signed_out", "This browser is not signed in (no session, or it ended).", "Sign in again."));
  const id = request.nextUrl.searchParams.get("id")?.toLowerCase() ?? "";
  if (!ID.test(id)) return failed(unknownSaving());
  return { owner: session.acct.uuid, id };
}

function answer(id: string, outcome: SavingOutcome): NextResponse {
  if (outcome.kind === "saved") return json(200, { type: "upload", data: outcome.upload });
  if (outcome.kind === "failed") return failed(outcome.failure);
  return json(202, { type: "upload.saving", data: { id } });
}

export async function GET(request: NextRequest): Promise<NextResponse> {
  const who = caller(request);
  if (who instanceof Response) return who;
  const raw = request.nextUrl.searchParams.get("wait");
  const seconds = raw === null ? SAVING_TIMING.waitMs / 1000 : Number(raw);
  const waitMs = Number.isFinite(seconds) ? Math.max(0, Math.min(SAVING_TIMING.waitMs, seconds * 1000)) : SAVING_TIMING.waitMs;
  const outcome = await askSaving(who.id, who.owner, waitMs, request.signal);
  return outcome ? answer(who.id, outcome) : failed(unknownSaving());
}

export async function DELETE(request: NextRequest): Promise<NextResponse> {
  const problem = sameOriginProblem(request);
  if (problem) return json(403, errorBody("cross_site_request", problem, "Use this site's own pages."));
  const who = caller(request);
  if (who instanceof Response) return who;
  const outcome = await cancelSaving(who.id, who.owner);
  if (!outcome) return failed(unknownSaving());
  // "cancelled" only when Briefcase let go of the reservation: a refused cancel is "stopped", never reported as done.
  const state = outcome.kind === "saved" ? "saved" : outcome.kind === "failed" ? (outcome.failure.reservationLeft ? "stopped" : "cancelled") : "cancelling";
  return json(200, { type: "upload.cancel", data: { id: who.id, state } });
}
