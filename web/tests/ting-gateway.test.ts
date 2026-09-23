import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer, type Server, type IncomingHttpHeaders } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import WebSocket from "ws";
import { configuration } from "../server/config.ts";
import { Gateway } from "../server/gateway.ts";
async function listen(server: Server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert(address && typeof address !== "string");
  return `http://127.0.0.1:${address.port}`;
}
test("Ting cutover retires the bridge and preserves selected HTTP authority", async (t) => {
  const calls: {
    path: string;
    method: string;
    headers: IncomingHttpHeaders;
    body: string;
  }[] = [];
  let upgrades = 0;
  const upstream = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk);
    calls.push({
      path: req.url!,
      method: req.method!,
      headers: req.headers,
      body: Buffer.concat(chunks).toString(),
    });
    res.setHeader("Content-Type", "application/json");
    if (req.method === "DELETE") {
      res.writeHead(204);
      res.end();
    } else
      res.end(JSON.stringify({ type: "response", data: { accepted: true } }));
  });
  upstream.on("upgrade", (_req, socket) => {
    upgrades++;
    socket.destroy();
  });
  const api = await listen(upstream);
  const directory = await mkdtemp(join(tmpdir(), "dm-ting-gateway-"));
  let gateway: Gateway;
  const server = createServer((req, res) => void gateway.handle(req, res));
  server.on("upgrade", (req, socket, head) =>
    gateway.upgrade(req, socket, head),
  );
  const origin = await listen(server);
  gateway = new Gateway(
    configuration({
      DM_WEB_ORIGIN: origin,
      DM_API_ORIGIN: api,
      DM_WEB_STATE_DIR: directory,
      DM_TING_BROWSER_ORIGIN: "https://ting.example",
    }),
  );
  await gateway.initialize();
  t.after(async () => {
    await gateway.close();
    await Promise.all([
      new Promise<void>((r) => server.close(() => r())),
      new Promise<void>((r) => upstream.close(() => r())),
    ]);
    await rm(directory, { recursive: true, force: true });
  });
  const profile = "00000000-0000-4000-8000-000000000021";
  const browser = await gateway.sessions.create();
  browser.value.selected = profile;
  browser.value.profiles = [
    {
      profile_id: profile,
      actor: { id: "c:test-carbon", type: "carbon" },
      organization_id: "tos",
      testing_environment_id: "00000000-0000-4000-8000-000000000022",
      testing_key: "fixture-plane-key",
      access_token: "fixture-access",
      refresh_token: "fixture-refresh",
      expires_at: Date.now() + 3600000,
    },
  ];
  await gateway.sessions.save(browser);
  const headers = {
    Origin: origin,
    Cookie: gateway.sessions.cookie(browser.id).split(";")[0]!,
    "X-DM-Profile": profile,
    "Content-Type": "application/json",
    "X-Testing-Environment-Generation": "7",
    "Idempotency-Key": "explicit-register-key",
  };
  for (const [path, method, body] of [
    ["iam", "GET", undefined],
    ["sync?cursor=opaque%2Bcursor&limit=100", "GET", undefined],
    [
      "delivery/registration",
      "POST",
      { type: "delivery_registration", data: {} },
    ],
    [
      "presence/devices/browser-device",
      "PUT",
      { type: "renew_presence", data: { activity: "typing" } },
    ],
    ["presence/devices/browser-device", "DELETE", undefined],
  ] as const) {
    const response = await fetch(`${origin}/api/dm/${path}`, {
      method,
      headers,
      body: body ? JSON.stringify(body) : undefined,
    });
    assert(response.ok, `${method} ${path}: ${response.status}`);
    await response.arrayBuffer();
    const call = calls.at(-1)!;
    assert.equal(call.path, `/api/v1/${path}`);
    assert.equal(call.method, method);
    assert.equal(call.headers.authorization, "Bearer fixture-access");
    assert.equal(call.headers["x-org-id"], "tos");
    assert.equal(
      call.headers["x-testing-environment-key"],
      "fixture-plane-key",
    );
    assert.equal(call.headers["x-testing-environment-generation"], "7");
    assert(!call.headers.cookie);
  }
  assert.equal(calls[2]!.headers["idempotency-key"], "explicit-register-key");
  assert.equal(
    calls[2]!.body,
    JSON.stringify({ type: "delivery_registration", data: {} }),
  );
  const before = calls.length;
  const config = await fetch(`${origin}/api/config`).then((r) => r.json());
  assert.equal(config.data.ting_browser_origin, "https://ting.example");
  const retired = await fetch(`${origin}/api/ws`);
  assert.equal(retired.status, 410);
  assert.equal(
    (await retired.json()).data.error.code,
    "delivery_moved_to_ting",
  );
  const upgrade = (requestOrigin: string) =>
    new Promise<number>((resolve, reject) => {
      const socket = new WebSocket(origin.replace("http", "ws") + "/api/ws", {
        origin: requestOrigin,
      });
      socket.once("open", () => {
        socket.terminate();
        reject(new Error("retired gateway upgraded"));
      });
      socket.on("error", () => {});
      socket.once("unexpected-response", (_request, response) => {
        response.resume();
        socket.terminate();
        resolve(response.statusCode!);
      });
    });
  assert.equal(await upgrade(origin), 410);
  assert.equal(await upgrade("https://untrusted.example"), 403);
  assert.equal(calls.length, before);
  assert.equal(upgrades, 0);
});
