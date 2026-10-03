import type { IncomingMessage, ServerResponse } from "node:http";
import type { Duplex } from "node:stream";
import { randomBytes } from "node:crypto";
import type { Config } from "./config.ts";
import { Auth } from "./auth.ts";
import {
  GatewayError,
  Sessions,
  constantEqual,
  publicProfile,
  selectProfile,
  uuidPattern,
} from "./session.ts";
import {
  failure,
  json,
  profileHeader,
  proxy,
  requestJson,
  responseHeaders,
} from "./proxy.ts";
import { rejectRetiredUpgrade } from "./websocket.ts";

function requestedProfile(
  req: IncomingMessage,
  body: Record<string, unknown>,
): string | undefined {
  const value = body.profile_id ?? profileHeader(req);
  if (
    value !== undefined &&
    (typeof value !== "string" || !uuidPattern.test(value))
  )
    throw new GatewayError(
      400,
      "invalid_profile",
      "Provide a valid browser profile ID.",
    );
  return value as string | undefined;
}
const corsHeaders = new Set([
  "x-dm-telemetry",
  "x-dm-source",
  "content-type",
  "x-dm-profile",
  "idempotency-key",
  "if-match",
  "x-testing-environment-generation",
]);
export class Gateway {
  readonly sessions: Sessions;
  readonly auth: Auth;
  private cleanup?: NodeJS.Timeout;
  private rate = new Map<string, { window: number; count: number }>();
  constructor(readonly config: Config) {
    this.sessions = new Sessions(config);
    // Incoming browser delivery belongs to Ting. DM keeps only HTTP sessions.
    this.auth = new Auth(config, this.sessions, () => {});
  }
  async initialize(): Promise<void> {
    await this.sessions.initialize();
    this.cleanup = setInterval(
      () => {
        void this.sessions.cleanup().catch(() => {});
      },
      60 * 60 * 1000,
    );
    this.cleanup.unref();
  }
  async close(): Promise<void> {
    clearInterval(this.cleanup);
    await this.sessions.close();
  }
  upgrade(req: IncomingMessage, socket: Duplex, head: Buffer): void {
    rejectRetiredUpgrade(req, socket, this.config);
  }
  private authRate(req: IncomingMessage): void {
    const now = Date.now();
    for (const [key, value] of this.rate)
      if (value.window < now - 60000) this.rate.delete(key);
    const key =
      this.sessions.cookieId(req) || req.socket.remoteAddress || "unknown";
    const entry = this.rate.get(key) || { window: now, count: 0 };
    entry.count++;
    this.rate.set(key, entry);
    if (entry.count > 60 || this.rate.size > 10000)
      throw new GatewayError(
        429,
        "login_rate_limit",
        "Wait a minute before retrying authentication.",
      );
  }
  async handle(req: IncomingMessage, res: ServerResponse): Promise<boolean> {
    const raw = req.url || "/";
    if (raw === "/healthz" && req.method === "GET") {
      json(res, { status: "ok" });
      return true;
    }
    if (!raw.startsWith("/api/") && !raw.startsWith("/auth/")) return false;
    try {
      responseHeaders(res);
      res.setHeader("Vary", "Origin");
      if (req.headers.origin === this.config.frontend.origin) {
        res.setHeader(
          "Access-Control-Allow-Origin",
          this.config.frontend.origin,
        );
        res.setHeader("Access-Control-Allow-Credentials", "true");
        res.setHeader(
          "Access-Control-Expose-Headers",
          "ETag, Retry-After, X-Request-ID",
        );
      }
      if (req.headers.host !== this.config.origin.host)
        throw new GatewayError(
          403,
          "untrusted_host",
          "This gateway host is not allowed.",
        );
      if (
        !raw.startsWith("/") ||
        raw.startsWith("//") ||
        /%(?:2f|5c|00)/i.test(raw.split("?")[0]!) ||
        raw.includes("\\")
      )
        throw new GatewayError(400, "invalid_path", "Invalid gateway path.");
      const url = new URL(raw, this.config.origin),
        path = url.pathname,
        method = req.method || "GET";
      const navigation =
        method === "GET" && ["/auth/login", "/auth/callback"].includes(path);
      const origin = req.headers.origin;
      if (method === "OPTIONS" && path.startsWith("/api/")) {
        const requestedMethod = req.headers["access-control-request-method"];
        const requestedHeaders = req.headers["access-control-request-headers"];
        if (
          origin !== this.config.frontend.origin ||
          typeof requestedMethod !== "string" ||
          !["GET", "POST", "PUT", "PATCH", "DELETE"].includes(
            requestedMethod,
          ) ||
          (requestedHeaders !== undefined &&
            (typeof requestedHeaders !== "string" ||
              requestedHeaders
                .split(",")
                .some((name) => !corsHeaders.has(name.trim().toLowerCase()))))
        )
          throw new GatewayError(
            403,
            "frontend_csrf",
            "This frontend request is not allowed.",
          );
        res.setHeader(
          "Vary",
          "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
        );
        res.setHeader(
          "Access-Control-Allow-Methods",
          "GET, POST, PUT, PATCH, DELETE",
        );
        res.setHeader(
          "Access-Control-Allow-Headers",
          [...corsHeaders].join(", "),
        );
        res.setHeader("Access-Control-Max-Age", "600");
        res.statusCode = 204;
        res.end();
        return true;
      }
      if (
        !navigation &&
        (req.headers["sec-fetch-site"] === "cross-site" ||
          (origin && origin !== this.config.frontend.origin) ||
          (!["GET", "HEAD"].includes(method) &&
            origin !== this.config.frontend.origin))
      )
        throw new GatewayError(
          403,
          "frontend_csrf",
          "This request must originate from the DM frontend.",
        );
      if (path === "/api/config" && method === "GET") {
        json(res, {
          iam_login_url: new URL("/auth/login", this.config.origin).href,
          app_id: this.config.appId,
          api_origin: this.config.api.origin,
          gateway_origin: this.config.origin.origin,
          frontend_origin: this.config.frontend.origin,
          max_body_bytes: this.config.maxBytes,
          ting_browser_origin: this.config.tingBrowser.origin,
        });
        return true;
      }
      if (path === "/api/ws")
        throw new GatewayError(
          410,
          "delivery_moved_to_ting",
          "DM browser delivery has moved to Ting. Use HTTP for messages, synchronization, presence and receipts.",
        );
      if (path === "/auth/retry.js" && method === "GET") {
        res.setHeader("Content-Type", "application/javascript; charset=utf-8");
        res.end(
          'document.querySelector("button").addEventListener("click",()=>location.reload());',
        );
        return true;
      }
      if (path === "/auth/login" && method === "GET") {
        this.authRate(req);
        const kind = url.searchParams.get("identity_kind"),
          nonce = url.searchParams.get("popup_nonce"),
          selected = url.searchParams.get("profile_id");
        if (
          ["identity_kind", "popup_nonce", "profile_id"].some(
            (key) => url.searchParams.getAll(key).length > 1,
          ) ||
          (kind !== null && kind !== "carbon" && kind !== "silicon") ||
          (nonce !== null && (!kind || !/^[a-f0-9]{64}$/.test(nonce))) ||
          (selected !== null && !uuidPattern.test(selected))
        )
          throw new GatewayError(
            400,
            "invalid_login",
            "Choose Carbon or Silicon to sign in.",
          );
        const previous = await this.sessions.read(this.sessions.cookieId(req));
        const browser = previous || (await this.sessions.create());
        const state = randomBytes(32).toString("base64url");
        await this.sessions.locked(browser.id, async () => {
          const current = (await this.sessions.read(browser.id)) || browser;
          if (selected && selected !== current.value.selected)
            throw new GatewayError(
              409,
              "context_changed",
              "The selected account changed. Start sign-in again.",
            );
          current.value.flow = {
            state: this.sessions.keyFor("login-state", state),
            deadline: Date.now() + 10 * 60 * 1000,
            identity_kind: kind || undefined,
            popup_nonce: nonce || undefined,
            selected: current.value.selected,
          };
          await this.sessions.save(current);
        });
        const callback = new URL("/auth/callback", this.config.origin);
        callback.searchParams.set("state", state);
        const target = new URL("/login", this.config.iam);
        target.searchParams.set("app_id", this.config.appId);
        if (kind) target.searchParams.set("identity_kind", kind);
        if (nonce) target.searchParams.set("display", "popup");
        target.searchParams.set("redirect_uri", callback.href);
        res.setHeader("Set-Cookie", this.sessions.cookie(browser.id));
        res.writeHead(303, { Location: target.href });
        res.end();
        return true;
      }
      if (path === "/auth/callback" && method === "GET") {
        this.authRate(req);
        const id = this.sessions.cookieId(req),
          state = url.searchParams.get("state"),
          slt = url.searchParams.get("slt");
        if (
          !id ||
          !state ||
          !slt ||
          url.searchParams.getAll("state").length !== 1 ||
          url.searchParams.getAll("slt").length !== 1
        )
          throw new GatewayError(
            400,
            "login_state",
            "Login callback is missing its browser binding. Start sign-in again.",
          );
        const completed = await this.sessions.locked(id, async () => {
          const browser = await this.sessions.read(id),
            flow = browser?.value.flow;
          if (
            !browser ||
            !flow ||
            flow.deadline <= Date.now() ||
            (flow.completed || flow.selected) !== browser.value.selected ||
            !constantEqual(
              flow.state,
              this.sessions.keyFor("login-state", state),
            )
          )
            throw new GatewayError(
              400,
              "login_state",
              "Login state is expired or belongs to another browser. Start sign-in again.",
            );
          const input = this.sessions.keyFor("login-callback-input", slt);
          if (flow.input && !constantEqual(flow.input, input))
            throw new GatewayError(
              409,
              "login_changed",
              "This sign-in attempt already belongs to a different callback.",
            );
          if (flow.completed) {
            const profile = selectProfile(browser, flow.completed);
            if (
              profile.auth_required ||
              (flow.identity_kind && profile.actor.type !== flow.identity_kind)
            )
              throw new GatewayError(
                409,
                "login_changed",
                "The completed sign-in is no longer available.",
              );
            return { nonce: flow.popup_nonce, profile: profile.profile_id };
          }
          flow.input = input;
          await this.sessions.save(browser);
          const profile = await this.auth.login(
            browser,
            { slt },
            flow.identity_kind,
            flow,
          );
          return { nonce: flow.popup_nonce, profile: profile.profile_id };
        });
        res.setHeader("Set-Cookie", this.sessions.cookie(id));
        const destination = new URL(this.config.frontend);
        if (completed.nonce) {
          destination.searchParams.set("iam_popup", "complete");
          destination.searchParams.set("nonce", completed.nonce);
          destination.searchParams.set("profile_id", completed.profile);
        }
        res.writeHead(303, { Location: destination.href });
        res.end();
        return true;
      }
      if (path === "/api/login/cancel" && method === "POST") {
        const body = await requestJson(req),
          id = this.sessions.cookieId(req);
        if (
          typeof body.nonce !== "string" ||
          !/^[a-f0-9]{64}$/.test(body.nonce)
        )
          throw new GatewayError(
            400,
            "invalid_login",
            "A login cancellation requires its popup nonce.",
          );
        if (id)
          await this.sessions.locked(id, async () => {
            const browser = await this.sessions.read(id),
              flow = browser?.value.flow;
            if (!browser || !flow || flow.popup_nonce !== body.nonce) return;
            if (flow.completed && browser.value.selected === flow.completed)
              browser.value.selected = browser.value.profiles.some(
                (p) => p.profile_id === flow.selected,
              )
                ? flow.selected
                : undefined;
            delete browser.value.flow;
            await this.sessions.save(browser);
          });
        json(res, { cancelled: true });
        return true;
      }
      if (path === "/api/login" && method === "POST") {
        this.authRate(req);
        const body = await requestJson(req);
        const existing = await this.sessions.read(this.sessions.cookieId(req)),
          browser = existing || (await this.sessions.create());
        await this.sessions.locked(browser.id, async () => {
          const current = (await this.sessions.read(browser.id)) || browser;
          await this.auth.login(current, body);
        });
        res.setHeader("Set-Cookie", this.sessions.cookie(browser.id));
        json(res, await this.auth.describe(browser.id));
        return true;
      }
      if (path === "/api/testing-environments/exit" && method === "POST") {
        const id = this.sessions.cookieId(req);
        if (!id)
          throw new GatewayError(401, "login_required", "Sign in to continue.");
        await this.sessions.locked(id, async () => {
          const browser = await this.sessions.read(id);
          if (!browser)
            throw new GatewayError(
              401,
              "login_required",
              "Sign in to continue.",
            );
          const production =
            browser.value.profiles.find(
              (p) =>
                p.profile_id === browser.value.production_profile_id &&
                !p.testing_environment_id,
            ) || browser.value.profiles.find((p) => !p.testing_environment_id);
          browser.value.selected = production?.profile_id;
          delete browser.value.flow;
          await this.sessions.save(browser);
        });
        json(res, await this.auth.describe(id));
        return true;
      }
      if (path === "/api/testing-environments/enter" && method === "POST") {
        this.authRate(req);
        const body = await requestJson(req),
          id = this.sessions.cookieId(req);
        const previous = await this.sessions.read(id);
        const profile = await this.auth.enterTestingEnvironment(
          id,
          profileHeader(req),
          body,
        );
        try {
          const session = await this.auth.describe(id, profile);
          if (!session.authenticated)
            throw new GatewayError(
              409,
              "testing_login_required",
              "This test account needs a new IAM test sign-in token.",
            );
          await this.sessions.locked(id!, async () => {
            const browser = await this.sessions.read(id);
            if (!browser)
              throw new GatewayError(
                401,
                "login_required",
                "Sign in to continue.",
              );
            selectProfile(browser, profile);
            browser.value.selected = profile;
            delete browser.value.flow;
            await this.sessions.save(browser);
          });
          json(res, session);
        } catch (error) {
          await this.sessions.locked(id!, async () => {
            const browser = await this.sessions.read(id);
            if (browser?.value.selected === profile) {
              browser.value.selected = previous?.value.selected;
              await this.sessions.save(browser);
            }
          });
          if (error instanceof GatewayError && error.status === 401)
            throw new GatewayError(
              409,
              "testing_login_required",
              "This test account needs a new IAM test sign-in token.",
            );
          throw error;
        }
        return true;
      }
      if (path === "/api/session" && method === "GET") {
        json(
          res,
          await this.auth.describe(
            this.sessions.cookieId(req),
            profileHeader(req),
          ),
        );
        return true;
      }
      if (path === "/api/profiles/select" && method === "POST") {
        const body = await requestJson(req),
          requested = requestedProfile(req, body),
          id = this.sessions.cookieId(req);
        if (!requested || !id)
          throw new GatewayError(
            401,
            "login_required",
            "Select an available browser profile.",
          );
        // Verify the target before changing the durable default. An upstream
        // outage must not make the next page load select a rejected switch.
        const verified = await this.auth.describe(id, requested);
        if (!verified.authenticated || !("actor" in verified))
          throw new GatewayError(
            401,
            "login_required",
            "Sign in to this browser profile.",
          );
        await this.sessions.locked(id, async () => {
          const browser = await this.sessions.read(id);
          if (!browser)
            throw new GatewayError(
              401,
              "login_required",
              "Sign in to continue.",
            );
          const target = selectProfile(browser, requested);
          // A concurrent logout, replacement or testing-context change may
          // have happened while the identity request was in flight.
          if (
            target.auth_required ||
            verified.profile_id !== target.profile_id ||
            verified.actor.id !== target.actor.id ||
            verified.actor.type !== target.actor.type ||
            verified.organization_id !== target.organization_id ||
            verified.testing_environment_id !== target.testing_environment_id
          )
            throw new GatewayError(
              409,
              "profile_changed",
              "This profile changed while it was being verified. Retry the switch.",
            );
          if (browser.value.selected !== requested) delete browser.value.flow;
          browser.value.selected = requested;
          await this.sessions.save(browser);
          verified.profiles = browser.value.profiles.map(publicProfile);
        });
        json(res, verified);
        return true;
      }
      if (path === "/api/refresh" && method === "POST") {
        const body = await requestJson(req),
          id = this.sessions.cookieId(req),
          requested = requestedProfile(req, body);
        await this.auth.fresh(id, requested, true);
        json(res, await this.auth.describe(id, requested));
        return true;
      }
      if (path === "/api/logout" && method === "POST") {
        const body = await requestJson(req),
          id = this.sessions.cookieId(req);
        await this.auth.logout(id, requestedProfile(req, body));
        json(res, await this.auth.describe(id));
        return true;
      }
      if (path.startsWith("/api/dm/")) {
        await proxy(req, res, url, this.config, this.auth);
        return true;
      }
      throw new GatewayError(
        404,
        "not_found",
        "This gateway endpoint does not exist.",
      );
    } catch (error) {
      // Keep the exact callback and durable attempt for an explicit retry.
      if (
        raw.split("?")[0] === "/auth/callback" &&
        error instanceof GatewayError &&
        (error.status === 429 || error.status >= 500)
      ) {
        res.writeHead(error.status, {
          "Content-Type": "text/html; charset=utf-8",
          "Referrer-Policy": "no-referrer",
          "Content-Security-Policy":
            "default-src 'none'; script-src 'self'; base-uri 'none'; frame-ancestors 'none'",
        });
        res.end(
          '<!doctype html><html><head><meta name="referrer" content="no-referrer"><title>Retry DM sign-in</title></head><body><h1>Sign-in is temporarily unavailable</h1><p>Your sign-in attempt is saved. Retry this same attempt when the service is ready.</p><button>Retry sign-in</button><script src="/auth/retry.js"></script></body></html>',
        );
        return true;
      }
      failure(res, error);
      return true;
    }
  }
}
