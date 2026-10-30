/**
 * GET /auth/callback?code=…&state=… (or ?error=…&state=…): the end of a sign-in. The state must be one this browser
 * started (the sealed pending cookie); the code is exchanged server side with the app secret and its PKCE verifier, and
 * the tokens are sealed into the session cookie. The browser then goes back to where it started.
 *
 * Failures land on /sign-in?error=<code>, which shows fixed words per code (error=access_denied, a Carbon who cancelled,
 * gets a friendly page): the address's own error_description is never shown, since anyone could put words in a link.
 * A session this sign-in replaces is revoked at Silicon Accounts, so no old sign-in lingers.
 */
import { NextResponse, type NextRequest } from "next/server";
import { EnvError, serverEnv } from "@/lib/server/env";
import { misconfigured, redirectTo } from "@/lib/server/responses";
import { clientHeaders, readPending, readSession, writePending, writeSession, type PendingSignIn } from "@/lib/server/session";
import { exchangeCode, revokeSession } from "@/lib/server/tokens";

export const dynamic = "force-dynamic";

// Next otherwise runs GET for HEAD, which would consume a code without setting a browser session.
export function HEAD() {
  return new NextResponse(null, { status: 204, headers: { "Cache-Control": "no-store" } });
}

/** What may be passed on to /sign-in as `error`: a code's shape, never a property every object has (`__proto__`). */
const ERROR_CODE = /^[a-z][a-z_]{0,47}$/;

function failed(publicUrl: string, code: string, pending: PendingSignIn[], state: string | null, returnTo?: string) {
  const target = new URL("/sign-in", publicUrl);
  target.searchParams.set("error", ERROR_CODE.test(code) ? code : "sign_in_failed");
  if (returnTo && returnTo !== "/") target.searchParams.set("return_to", returnTo);
  const response = redirectTo(target);
  writePending(response, pending.filter(entry => entry.s !== state));
  return response;
}

export async function GET(request: NextRequest) {
  if (request.headers.get("next-router-prefetch") || request.headers.get("purpose") === "prefetch" || request.headers.get("sec-purpose")?.includes("prefetch") || request.headers.get("rsc")) return HEAD();
  let env;
  try {
    env = serverEnv();
  } catch (error) {
    if (error instanceof EnvError) return misconfigured(error);
    throw error;
  }
  const params = request.nextUrl.searchParams;
  const state = params.get("state");
  const pending = readPending(request);
  const entry = state ? pending.find(item => item.s === state) : undefined;
  if (!entry) return failed(env.publicUrl, "state_mismatch", pending, state);
  const error = params.get("error");
  if (error) return failed(env.publicUrl, error, pending, state, entry.r);
  const code = params.get("code");
  if (!code) return failed(env.publicUrl, "missing_code", pending, state, entry.r);

  const headers = clientHeaders(request);
  const result = await exchangeCode(code, entry.v, headers);
  if (!result.ok) {
    console.warn(`sign-in: the code exchange failed: ${result.status} ${result.error}: ${result.description}`);
    return failed(env.publicUrl, result.error === "accounts_unreachable" ? "accounts_unreachable" : "exchange_failed", pending, state, entry.r);
  }
  const replaced = readSession(request);
  if (replaced && replaced.rt !== result.session.rt && (!replaced.sid || replaced.sid !== result.session.sid)) await revokeSession(replaced, headers);

  const response = redirectTo(new URL(entry.r, env.publicUrl));
  writeSession(response, result.session);
  writePending(response, pending.filter(item => item.s !== state));
  return response;
}
