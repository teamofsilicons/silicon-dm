import type { Config } from "./config.ts";
import {
  GatewayError,
  Sessions,
  publicProfile,
  selectProfile,
  uuidPattern,
  type Actor,
  type Browser,
  type Profile,
} from "./session.ts";
import { randomUUID } from "node:crypto";

type Tokens = {
  access_token: string;
  refresh_token: string;
  expires_in: number;
  actor: Actor;
  organization_id: string;
  organization_ids?: string[];
};
export function headersFor(profile: Profile): Headers {
  const headers = new Headers({
    Accept: "application/json",
    Authorization: `Bearer ${profile.access_token}`,
    "X-Org-ID": profile.organization_id,
  });
  if (profile.testing_key)
    headers.set("X-Testing-Environment-Key", profile.testing_key);
  return headers;
}
export async function responseJson(
  response: Response,
  limit = 64 * 1024,
): Promise<Record<string, unknown>> {
  const chunks: Uint8Array[] = [];
  let size = 0;
  const reader = response.body?.getReader();
  if (!reader)
    throw new GatewayError(
      502,
      "upstream_response",
      "The backend returned an invalid response.",
    );
  try {
    for (;;) {
      const part = await reader.read();
      if (part.done) break;
      size += part.value.byteLength;
      if (size > limit) {
        await reader.cancel();
        throw new Error("Response limit exceeded.");
      }
      chunks.push(part.value);
    }
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } finally {
    reader.releaseLock();
  }
}
export class Auth {
  constructor(
    readonly config: Config,
    readonly sessions: Sessions,
    readonly invalidate: (browser: string, profile: string) => void,
  ) {}
  private async authenticationRequest(
    path: string,
    body: Record<string, string>,
    testingKey?: string,
  ): Promise<Response> {
    const headers = new Headers({
      "Content-Type": "application/json",
      Accept: "application/json",
    });
    const secret = body.slt || body.refresh_token || body.token || "";
    headers.set(
      "Idempotency-Key",
      this.sessions.keyFor(`${path}|${testingKey || "production"}`, secret),
    );
    if (testingKey) headers.set("X-Testing-Environment-Key", testingKey);
    let response: Response;
    try {
      response = await fetch(new URL(path, this.config.api), {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        redirect: "error",
        signal: AbortSignal.timeout(20000),
      });
    } catch {
      throw new GatewayError(
        503,
        "authentication_unavailable",
        "Authentication could not complete. Retry the same request.",
      );
    }
    if (!response.ok) {
      await response.body?.cancel();
      if (response.status === 403)
        throw new GatewayError(
          403,
          "authorization_denied",
          "DM authorization for this profile was denied.",
        );
      const rejected = [400, 401].includes(response.status);
      throw new GatewayError(
        rejected ? 401 : 503,
        rejected ? "login_required" : "authentication_unavailable",
        rejected
          ? "The login or session is no longer valid. Sign in again."
          : "Authentication is temporarily unavailable. Your saved session is retained.",
      );
    }
    return response;
  }
  private async exchange(
    path: "/api/v1/auth/login" | "/api/v1/auth/refresh",
    body: Record<string, string>,
    testingKey?: string,
  ): Promise<Tokens> {
    const response = await this.authenticationRequest(path, body, testingKey);
    const value = await responseJson(response, 256 * 1024);
    const actor = value.actor as Actor | undefined;
    if (
      typeof value.access_token !== "string" ||
      typeof value.refresh_token !== "string" ||
      !value.access_token ||
      !value.refresh_token ||
      typeof value.expires_in !== "number" ||
      value.expires_in <= 0 ||
      !actor ||
      typeof actor.id !== "string" ||
      !["carbon", "silicon"].includes(actor.type) ||
      typeof value.organization_id !== "string"
    ) {
      throw new GatewayError(
        502,
        "upstream_response",
        "The backend returned an invalid session.",
      );
    }
    const organizations = value.organization_ids ?? [value.organization_id];
    if (
      !Array.isArray(organizations) ||
      !organizations.length ||
      organizations.length > 1000 ||
      !organizations.every(
        (org) => typeof org === "string" && /^[a-z0-9_-]{1,128}$/.test(org),
      ) ||
      !organizations.includes(value.organization_id)
    )
      throw new GatewayError(
        502,
        "upstream_response",
        "The backend returned invalid organization grants.",
      );
    return {
      ...value,
      organization_ids: [...new Set(organizations)],
    } as unknown as Tokens;
  }
  async login(
    browser: Browser,
    body: Record<string, unknown>,
  ): Promise<Profile> {
    if (
      typeof body.slt !== "string" ||
      body.slt.length < 8 ||
      body.slt.length > 8192 ||
      /\s/.test(body.slt)
    )
      throw new GatewayError(
        400,
        "invalid_login",
        "Provide the IAM short-lived token.",
      );
    const testingKey = body.testing_key,
      environment = body.testing_environment_id;
    if (
      (testingKey === undefined) !== (environment === undefined) ||
      (testingKey !== undefined &&
        (typeof testingKey !== "string" ||
          !/^[A-Za-z0-9]{32}$/.test(testingKey) ||
          typeof environment !== "string" ||
          !uuidPattern.test(environment)))
    ) {
      throw new GatewayError(
        400,
        "invalid_testing_environment",
        "Provide both the testing environment UUID and its root key.",
      );
    }
    if (new Set(browser.value.profiles.map((p) => p.refresh_token)).size >= 16)
      throw new GatewayError(
        409,
        "profile_limit",
        "Sign out of a browser profile before adding another.",
      );
    const tokens = await this.exchange(
      "/api/v1/auth/login",
      { slt: body.slt },
      testingKey as string | undefined,
    );
    const profiles = tokens.organization_ids!.map(
      (organization_id): Profile => {
        const previous = browser.value.profiles.find(
          (p) =>
            p.actor.id === tokens.actor.id &&
            p.actor.type === tokens.actor.type &&
            p.organization_id === organization_id &&
            p.testing_environment_id === environment,
        );
        return {
          profile_id: previous?.profile_id || randomUUID(),
          actor: tokens.actor,
          organization_id,
          access_token: tokens.access_token,
          refresh_token: tokens.refresh_token,
          expires_at: Date.now() + tokens.expires_in * 1000,
          testing_environment_id: environment as string | undefined,
          testing_key: testingKey as string | undefined,
        };
      },
    );
    const replaced = new Set(profiles.map((p) => p.profile_id));
    browser.value.profiles = browser.value.profiles.filter(
      (p) => !replaced.has(p.profile_id),
    );
    browser.value.profiles.push(...profiles);
    browser.value.selected = profiles[0]!.profile_id;
    delete browser.value.flow;
    await this.sessions.save(browser);
    for (const profile of profiles)
      this.invalidate(browser.id, profile.profile_id);
    return profiles[0]!;
  }
  async fresh(
    id: string | undefined,
    requested?: string,
    force = false,
  ): Promise<{ browser: Browser; profile: Profile }> {
    if (!id)
      throw new GatewayError(401, "login_required", "Sign in to continue.");
    return this.sessions.locked(id, async () => {
      const browser = await this.sessions.read(id);
      if (!browser)
        throw new GatewayError(401, "login_required", "Sign in to continue.");
      const profile = selectProfile(browser, requested);
      if (profile.auth_required)
        throw new GatewayError(
          401,
          "login_required",
          "Sign in to renew this browser profile.",
        );
      if (force || profile.expires_at <= Date.now() + 30000) {
        try {
          const tokens = await this.exchange(
            "/api/v1/auth/refresh",
            { refresh_token: profile.refresh_token },
            profile.testing_key,
          );
          if (
            tokens.actor.id !== profile.actor.id ||
            tokens.actor.type !== profile.actor.type
          )
            throw new GatewayError(
              502,
              "identity_changed",
              "Refresh returned an unexpected identity.",
            );
          // Each organization view shares one rotating IAM family. Update all
          // siblings atomically before any of them can attempt another refresh.
          const previousToken = profile.refresh_token;
          for (const sibling of browser.value.profiles) {
            if (sibling.refresh_token !== previousToken) continue;
            Object.assign(sibling, {
              access_token: tokens.access_token,
              refresh_token: tokens.refresh_token,
              expires_at: Date.now() + tokens.expires_in * 1000,
              auth_required: !tokens.organization_ids!.includes(
                sibling.organization_id,
              ),
            });
            if (sibling.auth_required) this.invalidate(id, sibling.profile_id);
          }
          await this.sessions.save(browser);
          if (profile.auth_required)
            throw new GatewayError(
              401,
              "login_required",
              "This organization is no longer authorized. Continue with IAM to update access.",
            );
        } catch (error) {
          if (error instanceof GatewayError && error.status === 401) {
            profile.auth_required = true;
            await this.sessions.save(browser);
            this.invalidate(id, profile.profile_id);
          }
          throw error;
        }
      }
      return { browser, profile };
    });
  }
  async expire(id: string, profile: Profile): Promise<void> {
    await this.sessions.locked(id, async () => {
      const browser = await this.sessions.read(id);
      const current = browser?.value.profiles.find(
        (p) => p.profile_id === profile.profile_id,
      );
      if (browser && current?.access_token === profile.access_token) {
        current.expires_at = 0;
        await this.sessions.save(browser);
      }
    });
  }
  async describe(id: string | undefined, requested?: string) {
    const existing = await this.sessions.read(id);
    if (!existing || !existing.value.profiles.length)
      return { authenticated: false, profiles: [] };
    let current: { browser: Browser; profile: Profile };
    try {
      current = await this.fresh(id, requested);
    } catch (error) {
      if (!(error instanceof GatewayError) || error.status !== 401) throw error;
      const saved = await this.sessions.read(id);
      return {
        authenticated: false,
        profile_id: requested || saved?.value.selected,
        profiles: (saved?.value.profiles || []).map(publicProfile),
      };
    }
    let response = await fetch(new URL("/api/v1/auth/me", this.config.api), {
      headers: headersFor(current.profile),
      redirect: "error",
      signal: AbortSignal.timeout(20000),
    });
    if (response.status === 401) {
      await response.body?.cancel();
      await this.expire(current.browser.id, current.profile);
      current = await this.fresh(
        current.browser.id,
        current.profile.profile_id,
      );
      response = await fetch(new URL("/api/v1/auth/me", this.config.api), {
        headers: headersFor(current.profile),
        redirect: "error",
        signal: AbortSignal.timeout(20000),
      });
    }
    if (!response.ok) {
      await response.body?.cancel();
      throw new GatewayError(
        response.status === 401 ? 401 : 503,
        "session_unavailable",
        "This session could not be verified. Retry or sign in again.",
      );
    }
    const identity = await responseJson(response);
    return {
      ...publicProfile(current.profile),
      authenticated: true,
      org_role: identity.org_role,
      capabilities: identity.capabilities,
      principal_id: identity.principal_id,
      profiles: current.browser.value.profiles.map(publicProfile),
    };
  }
  async logout(id: string | undefined, requested?: string): Promise<void> {
    if (!id) return;
    await this.sessions.locked(id, async () => {
      const browser = await this.sessions.read(id);
      if (!browser || !browser.value.profiles.length) return;
      const profile = selectProfile(browser, requested);
      try {
        const response = await this.authenticationRequest(
          "/api/v1/auth/logout",
          { token: profile.refresh_token },
          profile.testing_key,
        );
        await response.body?.cancel();
      } catch (error) {
        if (!(error instanceof GatewayError) || error.status !== 401)
          throw error;
      }
      const family = browser.value.profiles.filter(
        (p) => p.refresh_token === profile.refresh_token,
      );
      browser.value.profiles = browser.value.profiles.filter(
        (p) => p.refresh_token !== profile.refresh_token,
      );
      if (
        !browser.value.profiles.some(
          (p) => p.profile_id === browser.value.selected,
        )
      )
        browser.value.selected = browser.value.profiles[0]?.profile_id;
      await this.sessions.save(browser);
      for (const sibling of family) this.invalidate(id, sibling.profile_id);
    });
  }
}
