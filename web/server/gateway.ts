import type { IncomingMessage, ServerResponse } from "node:http";
import type { Duplex } from "node:stream";
import { randomBytes } from "node:crypto";
import type { Config } from "./config.ts";
import { Auth } from "./auth.ts";
import {
  GatewayError,
  Sessions,
  constantEqual,
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
import { Sockets } from "./websocket.ts";

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
  "content-type",
  "x-dm-profile",
  "idempotency-key",
  "if-match",
  "x-testing-environment-generation",
]);
export class Gateway {
  readonly sessions: Sessions;
  readonly sockets: Sockets;
  readonly auth: Auth;
  private cleanup?: NodeJS.Timeout;
  private rate = new Map<string, { window: number; count: number }>();
  constructor(readonly config: Config) {
    this.sessions = new Sessions(config);
    this.sockets = new Sockets(config);
    this.auth = new Auth(config, this.sessions, (browser, profile) =>
      this.sockets.invalidate(browser, profile),
    );
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
    this.sockets.close();
    await this.sessions.close();
  }
  upgrade(req: IncomingMessage, socket: Duplex, head: Buffer): void {
    void this.sockets.upgrade(req, socket, head, this.auth);
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
          default_organization_id: this.config.defaultOrganization,
          api_origin: this.config.api.origin,
          gateway_origin: this.config.origin.origin,
          frontend_origin: this.config.frontend.origin,
          max_body_bytes: this.config.maxBytes,
        });
        return true;
      }
      if (path === "/auth/login" && method === "GET") {
        this.authRate(req);
        const organization =
          url.searchParams.get("org_id") || this.config.defaultOrganization;
        if (
          !/^[a-z0-9_-]{1,128}$/.test(organization) ||
          url.searchParams.getAll("org_id").length > 1
        )
          throw new GatewayError(
            400,
            "invalid_organization",
            "Provide one organization ID for sign-in.",
          );
        const previous = await this.sessions.read(this.sessions.cookieId(req));
        const browser = previous || (await this.sessions.create());
        const state = randomBytes(32).toString("base64url");
        await this.sessions.locked(browser.id, async () => {
          const current = (await this.sessions.read(browser.id)) || browser;
          current.value.flow = {
            state: this.sessions.keyFor("login-state", state),
            deadline: Date.now() + 10 * 60 * 1000,
            organization_id: organization,
          };
          await this.sessions.save(current);
        });
        const callback = new URL("/auth/callback", this.config.origin);
        callback.searchParams.set("state", state);
        const target = new URL("/login", this.config.iam);
        target.searchParams.set("app_id", this.config.appId);
        target.searchParams.set("org_id", organization);
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
        await this.sessions.locked(id, async () => {
          const browser = await this.sessions.read(id),
            flow = browser?.value.flow;
          if (
            !browser ||
            !flow ||
            flow.deadline <= Date.now() ||
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
          await this.auth.login(browser, { slt }, flow.organization_id);
        });
        res.setHeader("Set-Cookie", this.sessions.cookie(id));
        res.writeHead(303, { Location: this.config.frontend.href });
        res.end();
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
        await this.sessions.locked(id, async () => {
          const browser = await this.sessions.read(id);
          if (!browser)
            throw new GatewayError(
              401,
              "login_required",
              "Sign in to continue.",
            );
          selectProfile(browser, requested);
          browser.value.selected = requested;
          await this.sessions.save(browser);
        });
        json(res, await this.auth.describe(id, requested));
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
      failure(res, error);
      return true;
    }
  }
}
