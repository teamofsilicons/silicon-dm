/**
 * GET /api/uploads/file?url=<a Silicon Briefcase public link>[&download=1]: a file from a message, served from DM's own
 * origin. Briefcase answers public bytes with Cross-Origin-Resource-Policy: same-origin, so an <img>, <audio> or
 * <video> on this site cannot load them from Briefcase directly (lib/upload/display.ts sends them here).
 *
 * It is not an open proxy: only the configured Briefcase's public files (/api/v1/public/{owner}/{path}), by their own
 * path, for a signed-in browser; a redirect is followed only to another public file of the same Briefcase. Range,
 * If-Range and If-None-Match go up; Content-Range, Content-Length, Accept-Ranges and ETag come back, so audio and video
 * seek. Uploaded bytes never run as DM: nosniff, a sandbox CSP, and inline only for raster images, audio, video and
 * PDF (everything else, SVG and HTML included, is a download).
 */
import { NextResponse, type NextRequest } from "next/server";
import { fileResponseHeaders, publicFileTarget, sameBriefcaseFile, uploadsSettings } from "@/lib/server/briefcase";
import { json } from "@/lib/server/responses";
import { errorBody, readSession } from "@/lib/server/session";

export const dynamic = "force-dynamic";

const FORWARD = ["range", "if-range", "if-none-match", "if-modified-since"];
const MAX_REDIRECTS = 3;
/**
 * How long Briefcase may take to start answering. The bytes themselves have no limit: a large file on a slow
 * connection takes as long as it takes, and stops when the browser goes away.
 */
const ANSWER_TIMEOUT_MS = 60_000;

async function handle(request: NextRequest): Promise<NextResponse> {
  const settings = uploadsSettings();
  if (!settings.enabled) return json(503, errorBody("uploads_not_configured", settings.reason, "Ask the sender for another link."));
  if (!readSession(request)) return json(401, errorBody("signed_out", "Files from messages are shown to signed-in browsers only.", "Sign in again."));
  const target = publicFileTarget(settings, request.nextUrl.searchParams.get("url"));
  if ("problem" in target) return json(400, errorBody("invalid_file_link", target.problem, "Open the file from its message."));
  const download = target.download || request.nextUrl.searchParams.get("download") === "1";

  const headers = new Headers({ accept: "*/*", "accept-encoding": "identity" });
  for (const name of FORWARD) {
    const value = request.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  // The browser going away stops everything; Briefcase not starting to answer in time stops the wait.
  const stop = new AbortController();
  const stopWith = () => stop.abort();
  request.signal.addEventListener("abort", stopWith, { once: true });
  const waiting = setTimeout(stopWith, ANSWER_TIMEOUT_MS);
  let url = target.upstream;
  let upstream: Response | null = null;
  try {
    for (let hop = 0; hop <= MAX_REDIRECTS; hop++) {
      upstream = await fetch(url, { method: request.method === "HEAD" ? "HEAD" : "GET", headers, redirect: "manual", cache: "no-store", signal: stop.signal });
      if (upstream.status < 300 || upstream.status >= 400 || upstream.status === 304) break;
      const next = sameBriefcaseFile(settings, upstream.headers.get("location") ?? "", url);
      void upstream.body?.cancel().catch(() => undefined);
      if (!next || hop === MAX_REDIRECTS) {
        clearTimeout(waiting);
        return json(502, errorBody("file_moved", "Silicon Briefcase sent this file somewhere DM does not follow.", "Open the file from Silicon Briefcase itself."));
      }
      url = next;
    }
  } catch {
    clearTimeout(waiting);
    return json(502, errorBody("briefcase_unavailable", "Silicon Briefcase did not answer, so the file could not be shown.", "Try again in a moment."));
  }
  clearTimeout(waiting);
  if (!upstream) return json(502, errorBody("briefcase_unavailable", "Silicon Briefcase did not answer, so the file could not be shown.", "Try again in a moment."));

  const status = upstream.status;
  if (status === 404 || status === 410 || status === 401 || status === 403) {
    void upstream.body?.cancel().catch(() => undefined);
    return json(404, errorBody("file_not_shared", "This file is no longer shared: its link was turned off, or the file was deleted.", "Ask the sender to send it again."));
  }
  if (status >= 500 || (status >= 400 && status !== 416)) {
    void upstream.body?.cancel().catch(() => undefined);
    return json(502, errorBody("briefcase_unavailable", `Silicon Briefcase could not give this file (HTTP ${status}).`, "Try again in a moment."));
  }
  const bodyless = request.method === "HEAD" || status === 304 || status === 204;
  if (bodyless) void upstream.body?.cancel().catch(() => undefined);
  return new NextResponse(bodyless ? null : upstream.body, { status, headers: fileResponseHeaders(upstream.headers, { download, fileName: target.fileName }) });
}

export const GET = handle;
export const HEAD = handle;
