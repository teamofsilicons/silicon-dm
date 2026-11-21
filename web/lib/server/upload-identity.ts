import { NextResponse, type NextRequest } from "next/server";
import { basicAuth, serverEnv } from "./env";
import { json } from "./responses";
import { errorBody, readSession, sameOriginProblem } from "./session";

export function interfaceUpload(request: NextRequest) {
  return (
    request.nextUrl.pathname === "/api/interface/uploads" ||
    request.nextUrl.pathname.startsWith("/api/interface/uploads/")
  );
}
export function uploadOriginProblem(request: NextRequest) {
  return interfaceUpload(request) ? null : sameOriginProblem(request);
}

type UploadIdentity = { at: string; ae: number; acct: { uuid: string } };
export async function uploadIdentity(request: NextRequest): Promise<UploadIdentity | NextResponse> {
  if (!interfaceUpload(request)) {
    return (
      readSession(request) ??
      json(401, errorBody("signed_out", "This browser is not signed in.", "Sign in to DM again."))
    );
  }
  const token = request.headers.get("authorization")?.match(/^Bearer ([^\s,]+)$/i)?.[1];
  const denied = () =>
    json(
      401,
      errorBody(
        "dm_token_required",
        "Use a current access token issued to DM.",
        "Renew the DM token through Silicon Accounts.",
      ),
    );
  if (!token || token.length > 8192) return denied();
  const env = serverEnv();
  try {
    const res = await fetch(`${env.accountsApiUrl}/v1/oauth/introspect`, {
      method: "POST",
      headers: { Authorization: basicAuth(env) },
      body: new URLSearchParams({ token }),
      cache: "no-store",
      redirect: "error",
      signal: AbortSignal.timeout(5000),
    });
    if (!res.ok) throw new Error("introspection unavailable");
    const live = await res.json();
    if (
      live.active !== true ||
      live.token_type !== "access_token" ||
      live.client_id !== env.appId ||
      typeof live.sub !== "string" ||
      !live.sub ||
      !Number.isFinite(live.exp) ||
      live.exp * 1000 <= Date.now()
    )
      return denied();
    return { at: token, ae: live.exp * 1000, acct: { uuid: live.sub } };
  } catch {
    return json(
      503,
      errorBody("accounts_unavailable", "Silicon Accounts could not verify this sign-in.", "Retry in a moment."),
    );
  }
}
