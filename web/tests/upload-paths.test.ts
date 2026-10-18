/**
 * Nothing in front of POST /api/uploads may read or limit the body: Next's proxy (proxy.ts) copies a request's body into
 * memory, up to experimental.proxyClientMaxBodySize, only for the paths it runs on, so it must never run on
 * /api/uploads (checked here with Next's own matcher compiler, the one the server uses). And the website's server is
 * set up for uploads of any length when it starts (instrumentation.ts → lib/server/briefcase.ts prepareServer): Node's
 * 5-minute deadline for a request's body is raised, and idle connections outlast a load balancer's, from the very first
 * request, an upload included. Checked on a real server, in the order `next start` runs: the server listens, register()
 * runs, then requests arrive. (Next loads a route's code only when its first request arrives, so a deadline set from
 * the route's module would come too late for that request: the first upload after a deploy was cut at 5 minutes.)
 */
import assert from "node:assert/strict";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { test } from "node:test";
import * as staticInfo from "next/dist/build/analysis/get-page-static-info";
import { getMiddlewareRouteMatcher } from "next/dist/shared/lib/router/utils/middleware-route-matcher";
import { register } from "../instrumentation";
import { KEEP_ALIVE_MS, REQUEST_DEADLINE_MS } from "../lib/server/briefcase";
import nextConfig from "../next.config";
import { config } from "../proxy";
import { applyTestEnv } from "./env";

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

interface Seen {
  path: string;
  requestTimeout: number;
  keepAliveTimeout: number;
  headersTimeout: number;
}

/** A server like Next's (Node's defaults), recording the server's timings as each request reaches its handler. */
async function nodeServer(): Promise<{ server: http.Server; base: string; seen: Seen[]; close: () => Promise<void> }> {
  const seen: Seen[] = [];
  const server = http.createServer((request, response) => {
    seen.push({ path: request.url ?? "", requestTimeout: server.requestTimeout, keepAliveTimeout: server.keepAliveTimeout, headersTimeout: server.headersTimeout });
    request.resume();
    request.on("end", () => response.end("ok"));
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  return {
    server,
    base,
    seen,
    close: async () => {
      server.closeAllConnections();
      await new Promise<void>(resolve => server.close(() => resolve()));
    },
  };
}

test("from its very first request, an upload included, the website's server gives a body a day and keeps idle connections longer than a load balancer: set up at start", async () => {
  applyTestEnv({ KEEP_ALIVE_TIMEOUT: undefined });
  process.env.NEXT_RUNTIME = "nodejs";
  const first = await nodeServer();
  try {
    assert.equal(first.server.requestTimeout, 300_000, "Node's default: a body still arriving after 5 minutes is cut with 408");
    assert.equal(first.server.keepAliveTimeout, 5_000, "Node's default: an idle connection is closed after 5 s");
    // The boot check prints one line (which Silicon Accounts, which service): not this test's business.
    const log = console.log;
    console.log = () => undefined;
    try {
      await register();
      await register();
    } finally {
      console.log = log;
    }
    const response = await fetch(`${first.base}/api/uploads?name=a.bin&size=3&sha256=${"0".repeat(64)}`, { method: "POST", body: "abc" });
    await response.text();
    assert.deepEqual(first.seen[0], { path: `/api/uploads?name=a.bin&size=3&sha256=${"0".repeat(64)}`, requestTimeout: REQUEST_DEADLINE_MS, keepAliveTimeout: KEEP_ALIVE_MS, headersTimeout: 60_000 }, "the very first request, an upload, already has the day; slow headers are still cut after 60 s");
    assert.equal(REQUEST_DEADLINE_MS, 24 * 60 * 60_000, "a day, never no deadline at all");
    assert.ok(KEEP_ALIVE_MS > 180_000, "longer than production's load balancer keeps its connections (180 s)");
    // Node writes the very first answer's Keep-Alive header before the request is announced, so that one header still
    // says 5 s; the connection itself is kept the new time (Node reads it when the answer ends), and later answers say so.
    const again = await fetch(`${first.base}/api/uploads/config`);
    await again.text();
    assert.match(again.headers.get("keep-alive") ?? "", new RegExp(`timeout=${KEEP_ALIVE_MS / 1000}\\b`));
    assert.equal(response.status, 200);

    // Any first request does it (the setting is the whole server's): a health check, a page.
    const second = await nodeServer();
    try {
      await (await fetch(`${second.base}/robots.txt`)).text();
      assert.equal(second.seen[0]!.requestTimeout, REQUEST_DEADLINE_MS);
      // An operator's own keep-alive (KEEP_ALIVE_TIMEOUT, which Next applies when the server is made) is left as it is.
      process.env.KEEP_ALIVE_TIMEOUT = "5000";
      const third = await nodeServer();
      try {
        await (await fetch(`${third.base}/c`)).text();
        assert.deepEqual([third.seen[0]!.requestTimeout, third.seen[0]!.keepAliveTimeout], [REQUEST_DEADLINE_MS, 5_000]);
      } finally {
        await third.close();
      }
    } finally {
      delete process.env.KEEP_ALIVE_TIMEOUT;
      await second.close();
    }
  } finally {
    await first.close();
  }
});
