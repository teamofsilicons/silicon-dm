import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import type { ReadableStream as NodeReadableStream } from "node:stream/web";
import type { Config } from "./config.ts";
import { Auth, headersFor } from "./auth.ts";
import { GatewayError } from "./session.ts";

const id = "[0-9a-fA-F-]{36}";
const allowed: [RegExp, string[]][] = [
  [/^auth\/me$/, ["GET"]],
  [/^conversations$/, ["GET", "POST"]],
  [new RegExp(`^conversations/${id}/messages$`), ["GET", "POST"]],
  [
    new RegExp(`^conversations/${id}/messages/${id}$`),
    ["GET", "PATCH", "DELETE"],
  ],
  [new RegExp(`^conversations/${id}/messages/${id}/receipts$`), ["POST"]],
  [new RegExp(`^conversations/${id}/draft$`), ["GET", "PUT", "DELETE"]],
  [new RegExp(`^conversations/${id}/bundles$`), ["POST"]],
  [new RegExp(`^conversations/${id}/bundles/${id}$`), ["GET"]],
  [/^presence\/[A-Za-z0-9_.:%>+-]+$/, ["GET"]],
  [/^gifs\/(?:trending|search|recent)$/, ["GET"]],
  [/^testing-environments$/, ["GET", "POST"]],
  [new RegExp(`^testing-environments/${id}$`), ["GET", "PATCH", "DELETE"]],
  [new RegExp(`^testing-environments/${id}/key$`), ["GET"]],
  [
    new RegExp(`^testing-environments/${id}/(?:rotate-key|restore|clean)$`),
    ["POST"],
  ],
];
export function responseHeaders(res: ServerResponse): void {
  res.setHeader("Cache-Control", "no-store");
  res.setHeader("Referrer-Policy", "no-referrer");
  res.setHeader("X-Content-Type-Options", "nosniff");
  res.setHeader("X-Frame-Options", "DENY");
}
export function json(res: ServerResponse, value: unknown, status = 200): void {
  responseHeaders(res);
  res.statusCode = status;
  res.setHeader("Content-Type", "application/json; charset=utf-8");
  res.end(JSON.stringify(value));
}
export function failure(res: ServerResponse, error: unknown): void {
  if (res.headersSent) {
    res.destroy();
    return;
  }
  const safe =
    error instanceof GatewayError
      ? error
      : new GatewayError(
          502,
          "gateway_unavailable",
          "The gateway could not complete this request. Retry with the same idempotency key.",
        );
  if (safe.status === 413) res.setHeader("Connection", "close");
  json(res, { error: { code: safe.code, message: safe.message } }, safe.status);
}
export async function requestJson(
  req: IncomingMessage,
): Promise<Record<string, unknown>> {
  if (
    req.headers["content-type"] &&
    !/^application\/json(?:\s*;|$)/i.test(req.headers["content-type"])
  )
    throw new GatewayError(415, "content_type", "Use application/json.");
  if (Number(req.headers["content-length"] || 0) > 16384)
    throw new GatewayError(
      413,
      "body_limit",
      "Authentication input is too large.",
    );
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req.iterator({ destroyOnReturn: false })) {
    size += chunk.length;
    if (size > 16384)
      throw new GatewayError(
        413,
        "body_limit",
        "Authentication input is too large.",
      );
    chunks.push(chunk);
  }
  try {
    const value = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error();
    return value;
  } catch {
    throw new GatewayError(400, "invalid_json", "Provide a JSON object.");
  }
}
export function profileHeader(req: IncomingMessage): string | undefined {
  const value = req.headers["x-dm-profile"];
  if (Array.isArray(value))
    throw new GatewayError(400, "invalid_profile", "Provide one profile ID.");
  return value;
}

/** Stream in both directions; never retain/replay an entire message request. */
export async function proxy(
  req: IncomingMessage,
  res: ServerResponse,
  url: URL,
  config: Config,
  auth: Auth,
): Promise<void> {
  const path = url.pathname.slice("/api/dm/".length),
    method = req.method || "GET";
  const route = allowed.find(([pattern]) => pattern.test(path));
  if (!route || !route[1].includes(method))
    throw new GatewayError(
      404,
      "not_found",
      "This DM operation is not exposed by the gateway.",
    );
  const { browser, profile } = await auth.fresh(
    auth.sessions.cookieId(req),
    profileHeader(req),
  );
  if (path.startsWith("testing-environments") && profile.testing_environment_id)
    throw new GatewayError(
      403,
      "production_profile_required",
      "Select a production profile to manage testing environments.",
    );
  const headers = headersFor(profile);
  for (const name of [
    "content-type",
    "idempotency-key",
    "if-match",
    "x-testing-environment-generation",
  ]) {
    const value = req.headers[name];
    if (typeof value === "string") headers.set(name, value);
  }
  if (path.startsWith("testing-environments"))
    headers.delete("x-testing-environment-generation");
  if (
    headers.has("x-testing-environment-generation") &&
    !/^[1-9][0-9]{0,15}$/.test(headers.get("x-testing-environment-generation")!)
  )
    throw new GatewayError(
      400,
      "invalid_generation",
      "Testing generation must be a positive integer.",
    );
  const declared = Number(req.headers["content-length"] || 0);
  if (!Number.isSafeInteger(declared) || declared < 0)
    throw new GatewayError(400, "invalid_length", "Invalid request length.");
  if (declared > config.maxBytes)
    throw new GatewayError(
      413,
      "body_limit",
      "The encoded request exceeds the gateway body limit.",
    );
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 180000);
  let size = 0,
    limited = false;
  const limiter = new Transform({
    transform(chunk: Buffer, _encoding, done) {
      size += chunk.length;
      if (size > config.maxBytes) {
        limited = true;
        done(new Error("body_limit"));
      } else done(null, chunk);
    },
  });
  const abort = () => controller.abort();
  req.once("aborted", abort);
  req.once("error", abort);
  res.once("close", abort);
  const withBody = !["GET", "HEAD"].includes(method);
  if (withBody) req.pipe(limiter);
  const target = new URL(`/api/v1/${path}${url.search}`, config.api);
  try {
    const init: RequestInit & { duplex?: "half" } = {
      method,
      headers,
      redirect: "manual",
      signal: controller.signal,
    };
    if (withBody) {
      init.body = Readable.toWeb(limiter) as ReadableStream<Uint8Array>;
      init.duplex = "half";
    }
    const response = await fetch(target, init);
    if (response.status >= 300 && response.status < 400) {
      await response.body?.cancel();
      throw new GatewayError(
        502,
        "upstream_redirect",
        "The backend returned an unexpected redirect.",
      );
    }
    if (response.status === 401) await auth.expire(browser.id, profile);
    responseHeaders(res);
    res.statusCode = response.status;
    for (const name of [
      "content-type",
      "retry-after",
      "etag",
      "x-request-id",
    ]) {
      const value = response.headers.get(name);
      if (value) res.setHeader(name, value);
    }
    if (!response.body) res.end();
    else
      await pipeline(
        Readable.fromWeb(response.body as NodeReadableStream<Uint8Array>),
        res,
      );
  } catch (error) {
    if (limited)
      throw new GatewayError(
        413,
        "body_limit",
        "The encoded request exceeds the gateway body limit.",
      );
    throw error;
  } finally {
    clearTimeout(timer);
    req.off("aborted", abort);
    req.off("error", abort);
    res.off("close", abort);
    if (withBody) {
      req.unpipe(limiter);
      limiter.destroy();
    }
  }
}
