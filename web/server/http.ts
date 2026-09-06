import type { IncomingMessage, ServerResponse } from "node:http";
import { createReadStream } from "node:fs";
import { realpath, stat } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { pipeline } from "node:stream/promises";
import type { Gateway } from "./gateway.ts";
import { failure, responseHeaders } from "./proxy.ts";
import { GatewayError } from "./session.ts";

export function handler(gateway: Gateway, assetDirectory: string) {
  const root = resolve(assetDirectory);
  if (
    gateway.config.directory === root ||
    gateway.config.directory.startsWith(root + sep)
  )
    throw new Error(
      "Private session state cannot be inside the asset directory.",
    );
  return async (req: IncomingMessage, res: ServerResponse): Promise<void> => {
    try {
      if (await gateway.handle(req, res)) return;
      if (req.headers.host !== gateway.config.origin.host)
        throw new GatewayError(
          403,
          "untrusted_host",
          "This gateway host is not allowed.",
        );
      if (!["GET", "HEAD"].includes(req.method || "GET"))
        throw new GatewayError(
          405,
          "method_not_allowed",
          "This resource is read-only.",
        );
      const url = new URL(req.url || "/", gateway.config.origin);
      let decoded: string;
      try {
        decoded = decodeURIComponent(url.pathname);
      } catch {
        throw new GatewayError(400, "invalid_path", "Invalid asset path.");
      }
      let path = resolve(root, `.${decoded}`);
      if (!path.startsWith(root + sep) && path !== root)
        throw new GatewayError(404, "not_found", "Resource not found.");
      try {
        if (!(await stat(path)).isFile()) path = resolve(root, "index.html");
      } catch {
        if (req.headers.accept?.includes("text/html") && !extname(path))
          path = resolve(root, "index.html");
        else throw new GatewayError(404, "not_found", "Resource not found.");
      }
      path = await realpath(path);
      if (!path.startsWith(root + sep))
        throw new GatewayError(404, "not_found", "Resource not found.");
      const info = await stat(path);
      const mime: Record<string, string> = {
        ".html": "text/html; charset=utf-8",
        ".js": "text/javascript; charset=utf-8",
        ".css": "text/css; charset=utf-8",
        ".svg": "image/svg+xml",
        ".woff2": "font/woff2",
        ".png": "image/png",
        ".ico": "image/x-icon",
        ".json": "application/json",
      };
      responseHeaders(res);
      res.setHeader(
        "Content-Type",
        mime[extname(path)] || "application/octet-stream",
      );
      res.setHeader("Content-Length", info.size);
      if (extname(path) === ".html")
        res.setHeader(
          "Content-Security-Policy",
          "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' https: data: blob:; media-src 'self' https: blob:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        );
      else if (path.includes(`${sep}assets${sep}`))
        res.setHeader("Cache-Control", "public, max-age=31536000, immutable");
      if (req.method === "HEAD") res.end();
      else await pipeline(createReadStream(path), res);
    } catch (error) {
      failure(res, error);
    }
  };
}
