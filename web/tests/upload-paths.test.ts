/**
 * Nothing in front of POST /api/uploads may read or limit the body: Next's proxy (proxy.ts) copies a request's body into
 * memory, up to experimental.proxyClientMaxBodySize, only for the paths it runs on, so it must never run on
 * /api/uploads (checked here with Next's own matcher compiler, the one the server uses). And Node's 5-minute deadline
 * for a request's body is lifted on the server that takes uploads (lib/server/briefcase.ts liftRequestDeadline),
 * checked on a real server.
 */
import assert from "node:assert/strict";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { test } from "node:test";
import * as staticInfo from "next/dist/build/analysis/get-page-static-info";
import { getMiddlewareRouteMatcher } from "next/dist/shared/lib/router/utils/middleware-route-matcher";
import { liftRequestDeadline } from "../lib/server/briefcase";
import nextConfig from "../next.config";
import { config } from "../proxy";

type Matchers = Parameters<typeof getMiddlewareRouteMatcher>[0];
/* The compiler Next runs on a proxy's `config.matcher` (exported at run time; its types leave it out). */
const { getMiddlewareMatchers } = staticInfo as unknown as { getMiddlewareMatchers: (matcher: unknown, nextConfig: { i18n: null; basePath: string }) => Matchers };

const runsOn = (() => {
  const match = getMiddlewareRouteMatcher(getMiddlewareMatchers(config.matcher, { i18n: null, basePath: "" }));
  return (path: string) => {
    const url = new URL(path, "http://dm.test");
    return match(url.pathname, { headers: {}, cookies: {} } as unknown as Parameters<typeof match>[1], Object.fromEntries(url.searchParams));
  };
})();

test("the proxy never runs on the upload routes (so Next never copies or limits their bodies), and still runs on pages", () => {
  for (const path of ["/api/uploads", "/api/uploads?name=a.pdf&size=10&sha256=abc", "/api/uploads/config", "/api/uploads/file?url=x", "/api/uploads/saving?id=x", "/api/api/v1/me"]) {
    assert.equal(runsOn(path), false, path);
  }
  for (const path of ["/", "/c", "/c/g%3Aproduct", "/settings", "/sign-in"]) {
    assert.equal(runsOn(path), true, path);
  }
});

test("a POST to a page can make the server hold at most 1 MB (Next copies a body for the proxy; no page reads one)", () => {
  assert.equal(nextConfig.experimental?.proxyClientMaxBodySize, "1mb");
});

test("Node's request deadline is lifted on the server that takes uploads, once a request for /api/uploads arrives", async () => {
  liftRequestDeadline();
  liftRequestDeadline();
  const server = http.createServer((_request, response) => response.end("ok"));
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  try {
    assert.equal(server.requestTimeout, 300_000, "Node's default: a body still arriving after 5 minutes is cut with 408");
    await (await fetch(`${base}/c`)).text();
    assert.equal(server.requestTimeout, 300_000, "other paths leave it alone");
    await (await fetch(`${base}/api/uploads/config`)).text();
    assert.equal(server.requestTimeout, 0, "no deadline for the body: an upload takes as long as it takes");
    assert.equal(server.headersTimeout, 60_000, "slow headers are still cut");
  } finally {
    server.closeAllConnections();
    await new Promise<void>(resolve => server.close(() => resolve()));
  }
});
