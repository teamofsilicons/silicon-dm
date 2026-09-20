/** Actual browser + DM client/gateway; all auth/data stay in a loopback fixture.
 * Run: node --experimental-transform-types scripts/verify-session-browser.ts
 * Open the printed URL. No production credentials or endpoints are used.
 */
import { createServer, type Server, type ServerResponse } from "node:http";
import { once } from "node:events";
import { mkdtemp, rm, writeFile, readdir, unlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";
import { build } from "esbuild";
import { WebSocketServer } from "ws";
import { Gateway } from "../server/gateway.ts";
import { configuration } from "../server/config.ts";

const directory = await mkdtemp(join(tmpdir(), "dm-browser-expiry-"));
const profileId = randomUUID();
const actor = { id: "session-test-carbon", type: "carbon" as const };
let tokenVersion = 0,
  accessValid = true,
  refreshStatus = 200;
let refreshAttempts = 0,
  messageAttempts = 0,
  socketConnections = 0;
const accepted = new Map<string, { body: string; data: object }>();
const attempts: { key: string; body: string; authorized: boolean }[] = [];
let report: unknown;
async function listen(server: Server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") throw Error("Missing listener");
  return `http://127.0.0.1:${address.port}`;
}
function json(res: ServerResponse, status: number, data: unknown) {
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Cache-Control": "no-store",
  });
  res.end(JSON.stringify({ type: status >= 400 ? "error" : "response", data }));
}
const upstream = createServer(async (req, res) => {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(chunk);
  const raw = Buffer.concat(chunks).toString();
  const body = raw ? JSON.parse(raw).data : {};
  if (req.url === "/api/v1/auth/refresh") {
    refreshAttempts++;
    if (refreshStatus !== 200)
      return json(res, refreshStatus, {
        error: {
          code: "fixture_refresh_failure",
          message: "Controlled refresh failure",
        },
      });
    if (body.refresh_token !== `fixture-refresh-${tokenVersion}`)
      return json(res, 401, {
        error: {
          code: "stale_refresh",
          message: "Refresh token already rotated",
        },
      });
    tokenVersion++;
    accessValid = true;
    return json(res, 200, {
      access_token: `fixture-access-${tokenVersion}`,
      refresh_token: `fixture-refresh-${tokenVersion}`,
      expires_in: 3600,
      actor,
      organization_id: "session-test",
    });
  }
  const authorized =
    accessValid &&
    req.headers.authorization === `Bearer fixture-access-${tokenVersion}`;
  if (req.url?.endsWith("/messages")) {
    messageAttempts++;
    attempts.push({
      key: String(req.headers["idempotency-key"] || ""),
      body: raw,
      authorized,
    });
  }
  if (!authorized)
    return json(res, 401, {
      error: {
        code: "unauthorized",
        message: "Controlled access-token expiry",
      },
    });
  if (req.url === "/api/v1/auth/me")
    return json(res, 200, { ...actor, org_role: "member", capabilities: [] });
  if (req.url?.endsWith("/messages")) {
    const key = String(req.headers["idempotency-key"] || "");
    if (!key) return json(res, 422, { error: { code: "missing_key" } });
    const previous = accepted.get(key);
    if (previous && previous.body !== raw)
      return json(res, 409, { error: { code: "idempotency_conflict" } });
    const data = previous?.data || {
      accepted: true,
      request_key: key,
      accepted_count: accepted.size + 1,
    };
    accepted.set(key, { body: raw, data });
    return json(res, 200, data);
  }
  return json(res, 200, {});
});
const apiOrigin = await listen(upstream);
const sockets = new WebSocketServer({ noServer: true });
upstream.on("upgrade", (req, socket, head) => {
  if (
    !accessValid ||
    req.headers.authorization !== `Bearer fixture-access-${tokenVersion}`
  ) {
    socket.end(
      "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
    );
    return;
  }
  sockets.handleUpgrade(req, socket, head, (ws) => {
    socketConnections++;
    ws.send(
      JSON.stringify({
        type: "connection.ready",
        data: {
          protocol_version: 5,
          connection_id: randomUUID(),
          members: [actor.id],
          acknowledged_through: {},
          testing_generation: null,
        },
      }),
    );
    ws.on("message", () => {});
  });
});
let gateway: Gateway;
let browser: Awaited<ReturnType<Gateway["sessions"]["create"]>>;
let clientScript = "";
const frontend = createServer(async (req, res) => {
  if (req.url === "/test") {
    res.writeHead(200, {
      "Content-Type": "text/html; charset=utf-8",
      "Set-Cookie": gateway.sessions.cookie(browser.id),
      "Cache-Control": "no-store",
    });
    return res.end(
      `<!doctype html><html><head><title>DM session recovery verification</title><style>body{font:17px system-ui;max-width:900px;margin:50px auto;padding:24px;background:#f5f7fa;color:#182334}li{margin:14px 0}pre{white-space:pre-wrap;background:white;padding:20px;border-radius:12px}.pass{color:#067342}.fail{color:#b21d31}</style></head><body><h1>DM session recovery verification</h1><p>Actual browser client, gateway, cookies, IndexedDB and WebSockets. Controlled loopback auth backend.</p><h2 id="status">Running…</h2><ol id="checks"></ol><pre id="details"></pre><script type="module" src="/session-test.js"></script></body></html>`,
    );
  }
  if (req.url === "/session-test.js") {
    res.writeHead(200, { "Content-Type": "text/javascript" });
    return res.end(clientScript);
  }
  if (req.url?.startsWith("/__test/")) {
    if (
      req.method !== "GET" &&
      req.headers.origin !== gateway.config.origin.origin
    )
      return json(res, 403, {});
    if (req.url === "/__test/expire-http") accessValid = false;
    if (req.url === "/__test/expire-ws") {
      accessValid = false;
      for (const ws of sockets.clients) ws.close(4001, "authorization-revoked");
    }
    if (req.url === "/__test/outage") {
      accessValid = false;
      refreshStatus = 503;
    }
    if (req.url === "/__test/restore") refreshStatus = 200;
    if (req.url === "/__test/revoke") {
      accessValid = false;
      refreshStatus = 401;
    }
    if (req.url === "/__test/complete") {
      const chunks: Buffer[] = [];
      for await (const chunk of req) chunks.push(chunk);
      report = JSON.parse(Buffer.concat(chunks).toString());
      await writeFile(
        join(directory, "result.json"),
        JSON.stringify(report, null, 2),
      );
      console.log(
        JSON.stringify({
          result: report,
          artifact: join(directory, "result.json"),
        }),
      );
    }
    const current = await gateway.sessions.read(browser.id);
    return json(res, 200, {
      refreshAttempts,
      messageAttempts,
      socketConnections,
      tokenVersion,
      acceptedCount: accepted.size,
      attempts,
      authenticated: !current?.value.profiles[0]?.auth_required,
      report,
    });
  }
  if (await gateway.handle(req, res)) return;
  res.writeHead(404);
  res.end();
});
const origin = await listen(frontend);
gateway = new Gateway(
  configuration({
    DM_WEB_ORIGIN: origin,
    DM_API_ORIGIN: apiOrigin,
    DM_WEB_STATE_DIR: directory,
  }),
);
await gateway.initialize();
frontend.on("upgrade", (req, socket, head) =>
  gateway.upgrade(req, socket, head),
);
browser = await gateway.sessions.create();
browser.value.selected = profileId;
browser.value.profiles = [
  {
    profile_id: profileId,
    actor,
    organization_id: "session-test",
    access_token: "fixture-access-0",
    refresh_token: "fixture-refresh-0",
    expires_at: Date.now() + 3600000,
  },
];
await gateway.sessions.save(browser);
const built = await build({
  stdin: {
    resolveDir: join(dirname(fileURLToPath(import.meta.url)), ".."),
    loader: "ts",
    contents: `
import {api,setSession} from "./src/api.ts";
import {connectRealtime} from "./src/realtime.ts";
localStorage.setItem("silicon-dm.telemetry","off");
const profileId=${JSON.stringify(profileId)};
const checks=[],states=[];let unauthorized=0,ready=0,connection;
window.addEventListener("dm:unauthorized",()=>unauthorized++);
const check=(name,condition)=>{if(!condition)throw Error(name);checks.push(name);const li=document.createElement("li");li.textContent="PASS: "+name;li.className="pass";document.querySelector("#checks").append(li);};
const wait=async(fn)=>{const until=Date.now()+15000;while(!fn()){if(Date.now()>until)throw Error("Timed out waiting for realtime recovery");await new Promise(r=>setTimeout(r,50));}};
const control=async(path)=>{const response=await fetch("/__test/"+path,{method:"POST"});return(await response.json()).data;};
const stats=async()=>(await(await fetch("/__test/stats")).json()).data;
const send=key=>api("/conversations/session-test-carbon::peer/messages",{method:"POST",body:{text:"Controlled local test message"},idempotencyKey:key});
try {
 const session=await api("/api/session",{profileId});setSession(session);
 check("Initial cookie session authenticated",session.authenticated);
 connection=connectRealtime(session,{onMessage(){},onReceipt(){},onReset(){},onReady(){ready++},onState(s){states.push(s)},onError(e){document.querySelector("#details").textContent=e.message}});
 await wait(()=>ready===1);check("Actual WebSocket connected",true);
 await control("expire-http");const sent=await send("browser-http-expiry");let result=await stats();
 check("Expired HTTP token refreshed without logout",sent.accepted&&result.refreshAttempts===1&&unauthorized===0);
 check("Rejected message retried exactly once and accepted once",result.messageAttempts===2&&result.acceptedCount===1);
 check("Retry preserved body and idempotency key",result.attempts[0].body===result.attempts[1].body&&result.attempts[0].key===result.attempts[1].key);
 await control("expire-ws");await wait(()=>ready===2);result=await stats();
 check("Expired WebSocket renewed and reconnected without logout",result.socketConnections===2&&result.tokenVersion===2&&unauthorized===0&&!states.includes("unauthorized"));
 await control("outage");let outage;try{await send("browser-outage-retry");}catch(e){outage=e;}result=await stats();
 check("Refresh outage retained session and rejected unsent action",outage?.status===503&&unauthorized===0&&result.authenticated&&result.acceptedCount===1);
 await control("restore");const recovered=await send("browser-outage-retry");result=await stats();
 check("Session recovered after outage without signing in",recovered.accepted&&result.acceptedCount===2&&unauthorized===0);
 await control("revoke");let revoked;try{await send("browser-revoked-send");}catch(e){revoked=e;}await wait(()=>unauthorized>0);result=await stats();
 check("Revoked refresh requires sign-in and never accepts the action",revoked?.status===401&&!result.authenticated&&result.acceptedCount===2);
 document.querySelector("#status").textContent="PASS — all "+checks.length+" checks";document.querySelector("#status").className="pass";
 const report={passed:true,checks,ready,states,unauthorized,refreshAttempts:result.refreshAttempts,acceptedMessages:result.acceptedCount};
 document.querySelector("#details").textContent=JSON.stringify(report,null,2);
 await fetch("/__test/complete",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(report)});
} catch(error) {
 document.querySelector("#status").textContent="FAIL: "+error.message;document.querySelector("#status").className="fail";
 await fetch("/__test/complete",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({passed:false,checks,states,unauthorized,error:error.message})});
} finally{connection?.close();}
`,
  },
  bundle: true,
  write: false,
  format: "esm",
  define: { "import.meta.env.VITE_DM_GATEWAY_ORIGIN": JSON.stringify(origin) },
});
clientScript = built.outputFiles[0]!.text;
console.log(
  JSON.stringify({ url: `${origin}/test`, stateDirectory: directory }),
);
async function close() {
  await gateway.close();
  for (const ws of sockets.clients) ws.terminate();
  sockets.close();
  frontend.closeAllConnections();
  upstream.closeAllConnections();
  await Promise.all([
    new Promise<void>((r) => frontend.close(() => r())),
    new Promise<void>((r) => upstream.close(() => r())),
  ]);
  for (const name of await readdir(directory))
    if (name !== "result.json") await unlink(join(directory, name));
  if (!report) await rm(directory, { recursive: true, force: true });
}
process.once("SIGINT", () => void close());
process.once("SIGTERM", () => void close());
