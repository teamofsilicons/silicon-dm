/**
 * POST /api/uploads and its streaming (lib/server/briefcase.ts) against a fake Silicon Briefcase and Silicon Accounts
 * on a loopback port (a real HTTP server, so what crosses the wire is what Briefcase would see): the bytes piped
 * through with the reservation's capability alone, the exact Content-Length and never chunked, with backpressure (the
 * body is never held: the browser is at most a few megabytes ahead of Briefcase); a fresh proof token for every
 * Briefcase request and the proof revoked; the reservation cancelled whenever the upload does not finish (the browser
 * gone, a broken or stalled transfer, a refusal); the refusals before any byte is read; names, types and slots.
 */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import http, { type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { afterEach, beforeEach, test } from "node:test";
import { NextRequest } from "next/server";
import { POST } from "../app/api/uploads/route";
import { DELETE as cancelSave, GET as askSave } from "../app/api/uploads/saving/route";
import { claimUploadSlot, cleanFileName, contentTypeFor, declaredFile, MAX_UPLOADS_PER_ACCOUNT, originalName, saveToBriefcase, SAVING_TIMING, sendBytes, storedName, UploadFailure, UploadProof, uploadsSettings, type EnabledUploads } from "../lib/server/briefcase";
import { serverEnv } from "../lib/server/env";
import { sealSession, type StoredSession } from "../lib/server/session";
import { applyTestEnv } from "./env";

const OWNER = "4143123f-b494-481c-adbf-c14b14cfccc0";
const UPLOAD = "6d1f3c1e-6f2b-4b8e-9a51-2a8d7d0b6f11";
const ENTRY = "a1e3c9d2-1b7f-4c55-8d4e-9f0a1b2c3d4e";
const ORIGIN = "http://127.0.0.1:4260";
const MB = 1024 * 1024;
const later = () => new Date(Date.now() + 600_000).toISOString();

const session = (overrides: Partial<StoredSession> = {}): StoredSession => ({
  v: 1, at: "eyJ.live.sig", rt: "sar_live", ae: Date.now() + 20 * 60_000, re: Date.now() + 86_400_000, scope: "profile",
  acct: { uuid: OWNER, kind: "carbon", id: "c:ada", display_name: "Ada", pfp_url: null, custodian: null }, ...overrides,
});

/** Deterministic bytes, made as they are read: the same `size` bytes every time for the same seed. */
function chunkAt(seed: number, index: number, length: number): Uint8Array {
  const out = new Uint8Array(length);
  let x = (seed * 2654435761 + index * 40503) >>> 0 || 1;
  for (let i = 0; i < length; i++) {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    out[i] = x & 0xff;
  }
  return out;
}

const CHUNK = 64 * 1024;

function digestOf(size: number, seed = 7): string {
  const hash = createHash("sha256");
  for (let at = 0, index = 0; at < size; at += CHUNK, index++) hash.update(chunkAt(seed, index, Math.min(CHUNK, size - at)));
  return hash.digest("hex");
}

interface Body {
  stream: ReadableStream<Uint8Array>;
  /** Bytes handed out so far. */
  produced: () => number;
}

/** A browser's body, made lazily (pulled only when the reader wants more); `failAt` breaks it there (a closed tab). */
function body(size: number, options: { seed?: number; failAt?: number } = {}): Body {
  let at = 0;
  let index = 0;
  const stream = new ReadableStream<Uint8Array>(
    {
      pull(controller) {
        if (options.failAt !== undefined && at >= options.failAt) {
          controller.error(new Error("the browser went away"));
          return;
        }
        if (at >= size) {
          controller.close();
          return;
        }
        const length = Math.min(CHUNK, size - at);
        controller.enqueue(chunkAt(options.seed ?? 7, index, length));
        index += 1;
        at += length;
      },
    },
    { highWaterMark: 0 },
  );
  return { stream, produced: () => at };
}

type TransferMode = "ok" | "refuse-early" | "refuse-at-end" | "break-mid" | "staged-then-break" | "stall";

interface Transfer {
  expect?: string | undefined;
  contentLength: string | undefined;
  transferEncoding: string | undefined;
  capability: string | undefined;
  authorization: string | undefined;
  received: number;
  sha256: string;
  complete: boolean;
  /** The website closed the connection while Briefcase was storing the file (answerDelayMs), before the answer. */
  closedBeforeAnswer?: boolean;
}

interface Fake {
  url: string;
  /** Every route called, in order ("issue", "refresh", "revoke" at Accounts; "reserve", "transfer"… at Briefcase). */
  routes: string[];
  /** The proof tokens Briefcase's JSON routes received, in order. */
  proofs: string[];
  revoked: string[];
  transfers: Transfer[];
  /** Bytes the transfer route has taken so far (for the backpressure check). */
  consumed: () => number;
  close: () => Promise<void>;
}

interface FakeOptions {
  transfer?: TransferMode;
  /** Milliseconds the transfer route waits between the chunks it reads (a slow Briefcase). */
  readDelayMs?: number;
  /** Milliseconds the transfer route takes after the last byte before it answers (Briefcase storing a large file). */
  answerDelayMs?: number;
  /** Replaces a JSON route's answer: [status, body]. */
  answer?: (route: string, attempt: number) => [number, unknown] | null;
}

const readJson = async (req: IncomingMessage) => {
  let text = "";
  for await (const chunk of req) text += chunk;
  return text ? (JSON.parse(text) as Record<string, unknown>) : {};
};

const send = (res: ServerResponse, status: number, value?: unknown) => {
  const text = value === undefined ? "" : JSON.stringify(value);
  res.writeHead(status, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
  res.end(text);
};

async function fakeServices(options: FakeOptions = {}): Promise<Fake> {
  const routes: string[] = [];
  const proofs: string[] = [];
  const revoked: string[] = [];
  const transfers: Transfer[] = [];
  const attempts = new Map<string, number>();
  let refreshes = 0;
  let consumed = 0;
  let state = "reserved";
  let manifest: Record<string, unknown> = {};
  const server = http.createServer();
  // Every request asking 100-continue is handled here (Node then leaves the 100 to the handler, as hyper does).
  server.on("checkContinue", (req, res) => server.emit("request", req, res));
  server.on("request", async (req: IncomingMessage, res: ServerResponse) => {
    const path = new URL(req.url ?? "/", "http://fake").pathname;
    if (path.startsWith("/v1/proofs/")) {
      const value = await readJson(req);
      if (path === "/v1/proofs/user-verification") {
        routes.push("issue");
        assert.equal(value.receiving_app, "briefcase");
        assert.deepEqual(value.scopes, ["briefcase.uploads.reserve", "briefcase.uploads.commit", "briefcase.uploads.status", "briefcase.uploads.cancel", "briefcase.link_access.update"]);
        assert.ok(req.headers["idempotency-key"], "issuing carries an Idempotency-Key");
        const replaced = options.answer?.("issue", 0);
        return replaced ? send(res, ...replaced) : send(res, 201, { proof_id: "p1", proof_token: "sap_1", proof_refresh_token: "sapr_1", expires_at: later() });
      }
      if (path === "/v1/proofs/refresh") {
        routes.push("refresh");
        refreshes += 1;
        assert.equal(value.proof_refresh_token, `sapr_${refreshes}`, "always the newest refresh token");
        return send(res, 200, { proof_id: "p1", proof_token: `sap_${refreshes + 1}`, proof_refresh_token: `sapr_${refreshes + 1}`, expires_at: later() });
      }
      if (path === "/v1/proofs/revoke") {
        routes.push("revoke");
        revoked.push(String(value.proof_id));
        return send(res, 204);
      }
    }
    if (path === `/api/v1/obo/uploads/${UPLOAD}/content` && req.method === "PUT") {
      routes.push("transfer");
      const transfer: Transfer = {
        contentLength: req.headers["content-length"],
        transferEncoding: req.headers["transfer-encoding"],
        capability: req.headers["x-briefcase-upload-capability"] as string | undefined,
        authorization: req.headers.authorization,
        received: 0,
        sha256: "",
        complete: false,
      };
      transfers.push(transfer);
      const mode = options.transfer ?? "ok";
      transfer.expect = req.headers.expect;
      // Like Briefcase (hyper sends 100 Continue only once the handler reads the body): a refusal from the headers goes
      // out before any byte is sent; otherwise the bytes are asked for.
      if (mode === "refuse-early") return send(res, 409, { error: { code: "delegated_upload_not_reserved", message: "The request conflicts with the current resource state." } });
      if (req.headers.expect === "100-continue") res.writeContinue();
      const hash = createHash("sha256");
      const size = Number(manifest.size);
      try {
        for await (const chunk of req as AsyncIterable<Buffer>) {
          transfer.received += chunk.length;
          consumed += chunk.length;
          hash.update(chunk);
          if (mode === "break-mid" && transfer.received >= size / 2) {
            req.socket.destroy();
            return;
          }
          if (mode === "stall") {
            req.pause();
            return;
          }
          if (options.readDelayMs) await new Promise(resolve => setTimeout(resolve, options.readDelayMs));
        }
      } catch {
        return;
      }
      transfer.sha256 = hash.digest("hex");
      transfer.complete = true;
      if (options.answerDelayMs) {
        // Briefcase storing the file: nothing moves; a website that gives up closes the connection meanwhile.
        const closed = await new Promise<boolean>(resolve => {
          const timer = setTimeout(() => resolve(false), options.answerDelayMs);
          req.socket.once("close", () => {
            clearTimeout(timer);
            resolve(true);
          });
        });
        if (closed) {
          transfer.closedBeforeAnswer = true;
          return;
        }
      }
      if (mode === "refuse-at-end" || transfer.received !== size || transfer.sha256 !== manifest.sha256) return send(res, 422, { error: { code: "delegated_upload_manifest_mismatch", message: "x" } });
      state = "staged";
      if (mode === "staged-then-break") {
        res.socket?.destroy();
        return;
      }
      return send(res, 200, { operation_id: manifest.operation_id, upload_id: UPLOAD, state, expires_at: later(), published_entry_id: null });
    }
    if (path.startsWith("/api/v1/obo/")) {
      const route = path.slice("/api/v1/obo/".length);
      const name = route === "uploads/reserve" ? "reserve" : route === "uploads/commit" ? "commit" : route === "uploads/status" ? "status" : route === "uploads/cancel" ? "cancel" : route;
      routes.push(name);
      const authorization = String(req.headers.authorization ?? "");
      assert.match(authorization, /^Proof sap_\d+$/);
      proofs.push(authorization);
      const value = await readJson(req);
      const attempt = attempts.get(name) ?? 0;
      attempts.set(name, attempt + 1);
      const replaced = options.answer?.(name, attempt);
      if (replaced) return send(res, ...replaced);
      const status = (published: string | null = null) => ({ operation_id: manifest.operation_id, upload_id: UPLOAD, state, expires_at: later(), published_entry_id: published });
      if (name === "reserve") {
        manifest = value;
        assert.equal(value.parent_path, "", "apps/dm, the app's own folder");
        assert.match(String(value.name), /^\d{8}-\d{6}-[0-9a-z]{6}-/, "a name of its own: Briefcase keeps one file per path");
        return send(res, 200, { ...status(), capability: "bcu_cap" });
      }
      if (name === "status") return send(res, 200, status());
      if (name === "commit") {
        assert.equal(value.upload_id, UPLOAD);
        if (state !== "staged") return send(res, 409, { error: { code: "delegated_upload_not_staged", message: "x" } });
        state = "committed";
        return send(res, 200, status(ENTRY));
      }
      if (name === "cancel") {
        if (state !== "committed") state = "cancelled";
        return send(res, 200, status());
      }
      if (name === "link-access") {
        assert.equal(value.entry_id, ENTRY);
        return send(res, 200, { url: `https://briefcase.test/c:ada/apps/webkit/${encodeURIComponent(String(manifest.name))}`, enabled: true, effective: true, inherited_from: null, can_manage: true, expires_at: null });
      }
    }
    send(res, 404, { error: { code: "not_found", message: `fake: no ${req.method} ${path}` } });
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  applyTestEnv({ ACCOUNTS_API_URL: url, BRIEFCASE_API_URL: url, UPLOADS_MAX_BYTES: undefined });
  return {
    url,
    routes,
    proofs,
    revoked,
    transfers,
    consumed: () => consumed,
    close: () =>
      new Promise<void>(resolve => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

let fake: Fake | null = null;
const SAVING_DEFAULTS = { ...SAVING_TIMING };
beforeEach(() => applyTestEnv({ UPLOADS_MAX_BYTES: undefined }));
afterEach(async () => {
  await fake?.close();
  fake = null;
  Object.assign(SAVING_TIMING, SAVING_DEFAULTS);
  applyTestEnv({ BRIEFCASE_API_URL: undefined, UPLOADS_MAX_BYTES: undefined });
});

interface UploadInit {
  name?: string;
  size?: number;
  sha256?: string;
  /** The body (default: `size` deterministic bytes). */
  body?: Body;
  headers?: Record<string, string | null>;
  query?: Record<string, string | null>;
  session?: StoredSession | null;
  signal?: AbortSignal;
}

function upload(init: UploadInit = {}): NextRequest {
  const size = init.size ?? 300_000;
  const query = new URLSearchParams({ name: init.name ?? "Quarterly plan.pdf", size: String(size), sha256: init.sha256 ?? digestOf(size) });
  for (const [key, value] of Object.entries(init.query ?? {})) {
    if (value === null) query.delete(key);
    else query.set(key, value);
  }
  const value = init.session === undefined ? session() : init.session;
  const headers: Record<string, string> = { "content-type": "application/pdf", "content-length": String(size), origin: ORIGIN, "sec-fetch-site": "same-origin", ...(value ? { cookie: `webkit_session=${sealSession(value)}` } : {}) };
  for (const [key, header] of Object.entries(init.headers ?? {})) {
    if (header === null) delete headers[key];
    else headers[key] = header;
  }
  const options = { method: "POST", body: (init.body ?? body(size)).stream, headers, duplex: "half", ...(init.signal ? { signal: init.signal } : {}) };
  return new NextRequest(`${ORIGIN}/api/uploads?${query}`, options as ConstructorParameters<typeof NextRequest>[1]);
}

const errorOf = async (response: Response) => ((await response.json()) as { error: { code: string; message: string; hint?: string } }).error;

/** An ask about (or a cancel of) a save, as the browser sends it. */
function saving(method: "GET" | "DELETE", id: string, init: { wait?: number; session?: StoredSession; headers?: Record<string, string> } = {}): NextRequest {
  const query = new URLSearchParams({ id, ...(init.wait !== undefined ? { wait: String(init.wait) } : {}) });
  const headers: Record<string, string> = { origin: ORIGIN, "sec-fetch-site": "same-origin", cookie: `webkit_session=${sealSession(init.session ?? session())}`, ...init.headers };
  return new NextRequest(`${ORIGIN}/api/uploads/saving?${query}`, { method, headers });
}

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

test("an upload: proof, reserve, the bytes piped through exactly (Content-Length, never chunked, the capability alone), commit, link; a fresh proof token each time; the proof revoked", async () => {
  fake = await fakeServices();
  const size = 3 * MB + 17;
  const response = await POST(upload({ size }));
  assert.equal(response.status, 200, JSON.stringify(await response.clone().json()));
  const answer = (await response.json()) as { type: string; data: { url: string; name: string; content_type: string; size: number } };
  assert.equal(answer.type, "upload");
  assert.deepEqual({ name: answer.data.name, type: answer.data.content_type, size: answer.data.size }, { name: "Quarterly plan.pdf", type: "application/pdf", size });
  assert.match(answer.data.url, new RegExp(`^https://127\\.0\\.0\\.1:\\d+/api/v1/public/${OWNER}/apps/webkit/\\d{8}-\\d{6}-[0-9a-z]{6}-Quarterly%20plan\\.pdf\\?view=inline$`), "the owner by uuid, the public inline view (a loopback stand-in is written with https)");
  assert.deepEqual(fake.routes, ["issue", "reserve", "transfer", "refresh", "commit", "refresh", "link-access", "revoke"]);
  assert.deepEqual(fake.proofs, ["Proof sap_1", "Proof sap_2", "Proof sap_3"], "never the same proof token twice");
  const [transfer] = fake.transfers;
  assert.equal(transfer!.contentLength, String(size), "the exact Content-Length");
  assert.equal(transfer!.transferEncoding, undefined, "never chunked");
  assert.equal(transfer!.capability, "bcu_cap");
  assert.equal(transfer!.authorization, undefined, "the bytes carry no account credential");
  assert.equal(transfer!.received, size);
  assert.equal(transfer!.sha256, digestOf(size), "every byte arrived as the browser sent it");
  assert.deepEqual(fake.revoked, ["p1"]);
  assert.equal(response.headers.get("cache-control"), "no-store");
});

test("backpressure: a slow Briefcase slows the browser; the body is never held (the browser stays only a few megabytes ahead)", async () => {
  fake = await fakeServices({ readDelayMs: 2 });
  const size = 24 * MB;
  const source = body(size);
  let lead = 0;
  const watch = setInterval(() => (lead = Math.max(lead, source.produced() - fake!.consumed())), 5);
  try {
    const response = await POST(upload({ size, body: source }));
    assert.equal(response.status, 200);
  } finally {
    clearInterval(watch);
  }
  assert.equal(fake.transfers[0]!.sha256, digestOf(size));
  assert.ok(lead < 8 * MB, `the browser was at most ${(lead / MB).toFixed(1)} MB ahead of Briefcase (of ${size / MB} MB)`);
});

test("the browser goes away mid-transfer: the transfer stops and the reservation is cancelled with a proof token of its own", async () => {
  fake = await fakeServices();
  const size = 4 * MB;
  const response = await POST(upload({ size, body: body(size, { failAt: MB }) }));
  assert.equal(response.status, 400);
  assert.equal((await errorOf(response)).code, "upload_interrupted");
  assert.deepEqual(fake.routes.filter(route => route !== "refresh"), ["issue", "reserve", "transfer", "cancel", "revoke"]);
  assert.equal(fake.transfers[0]!.complete, false);
  assert.equal(new Set(fake.proofs).size, fake.proofs.length, "the cancel has its own fresh token");
});

test("the browser's connection closes mid-transfer (the request's signal): the same, cancelled", async () => {
  fake = await fakeServices({ readDelayMs: 1 });
  const size = 16 * MB;
  const leaving = new AbortController();
  const watch = setInterval(() => {
    if (fake!.consumed() > 2 * MB) leaving.abort();
  }, 2);
  const response = await POST(upload({ size, signal: leaving.signal }));
  clearInterval(watch);
  assert.equal(response.status, 400);
  assert.equal((await errorOf(response)).code, "upload_interrupted");
  assert.ok(fake.routes.includes("cancel"));
  assert.ok(fake.transfers[0]!.received < size, "Briefcase stopped getting bytes");
});

test("Briefcase refuses the transfer from its headers: the refusal arrives before a byte is sent (Expect: 100-continue), the browser's body is left unread, cancelled", async () => {
  fake = await fakeServices({ transfer: "refuse-early" });
  const size = 48 * MB;
  const source = body(size);
  const response = await POST(upload({ size, body: source }));
  assert.equal(response.status, 503);
  const error = await errorOf(response);
  assert.equal(error.code, "briefcase_unavailable");
  assert.match(error.message, /no longer waiting for this file's bytes/);
  assert.equal(fake.transfers[0]!.expect, "100-continue");
  assert.equal(fake.transfers[0]!.received, 0, "no byte went to Briefcase");
  assert.ok(fake.routes.includes("cancel"));
  // The website then drops what the browser still sends for a moment (at most 32 MB), so it hears the answer, and lets go.
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.ok(source.produced() <= 32 * MB + CHUNK, `the website dropped ${(source.produced() / MB).toFixed(1)} MB of the browser's ${size / MB} MB, then let go`);
});

test("Briefcase refuses the bytes once it has them all (their SHA-256 is not the one prepared): the file changed, cancelled", async () => {
  fake = await fakeServices({ transfer: "refuse-at-end" });
  const response = await POST(upload({ size: 2 * MB }));
  assert.equal(response.status, 409);
  const error = await errorOf(response);
  assert.equal(error.code, "file_changed");
  assert.match(error.hint ?? "", /Choose the file again/);
  assert.ok(fake.routes.includes("cancel"));
});

test("the connection to Briefcase breaks part way: its status says the bytes did not all arrive, so it is cancelled (503, try again)", async () => {
  fake = await fakeServices({ transfer: "break-mid" });
  const response = await POST(upload({ size: 4 * MB }));
  assert.equal(response.status, 503);
  const error = await errorOf(response);
  assert.equal(error.code, "briefcase_unavailable");
  assert.match(error.message, /stopped taking the file part way/);
  assert.deepEqual(fake.routes.filter(route => route !== "refresh"), ["issue", "reserve", "transfer", "status", "cancel", "revoke"]);
});

test("the answer is lost after the last byte, but the status says staged: the upload is committed after all", async () => {
  fake = await fakeServices({ transfer: "staged-then-break" });
  const response = await POST(upload({ size: 2 * MB }));
  assert.equal(response.status, 200, JSON.stringify(await response.clone().json()));
  assert.deepEqual(fake.routes.filter(route => route !== "refresh"), ["issue", "reserve", "transfer", "status", "commit", "link-access", "revoke"]);
});

test("a transfer where nothing moves for the stall time stops, and the reservation is cancelled (408)", async () => {
  fake = await fakeServices({ transfer: "stall" });
  const settings = uploadsSettings() as EnabledUploads;
  const size = 16 * MB;
  const proof = await UploadProof.issue(serverEnv(), "eyJ.live.sig");
  await assert.rejects(
    saveToBriefcase({ settings, proof, name: storedName("still.bin"), contentType: "application/octet-stream", file: { size, sha256: digestOf(size) }, body: body(size).stream, owner: OWNER, appId: "webkit", timing: { stallMs: 300, answerMs: 1000, continueMs: 1000 } }),
    (error: unknown) => error instanceof UploadFailure && error.status === 408 && error.code === "upload_stalled",
  );
  assert.deepEqual(fake.routes.filter(route => route !== "refresh").slice(-2), ["status", "cancel"], "where it stands is asked first, then it is cancelled");
});

test("sendBytes on its own: exactly the declared length, or the browser's failure (more or fewer bytes never reach Briefcase as a file)", async () => {
  fake = await fakeServices();
  const settings = uploadsSettings() as EnabledUploads;
  const proof = await UploadProof.issue(serverEnv(), "eyJ.live.sig");
  // A reservation for 1 MB, then a body one byte short of it.
  const short = MB - 1;
  const reserve = await fetch(`${settings.apiBase}/api/v1/obo/uploads/reserve`, { method: "POST", headers: { authorization: `Proof ${await proof.fresh()}`, "content-type": "application/json" }, body: JSON.stringify({ operation_id: crypto.randomUUID(), parent_path: "", name: storedName("short.bin"), content_type: "application/octet-stream", size: MB, sha256: digestOf(MB) }) });
  assert.equal(reserve.status, 200);
  const outcome = await sendBytes(`${settings.apiBase}/api/v1/obo/uploads/${UPLOAD}/content`, "bcu_cap", MB, body(short).stream);
  assert.equal(outcome.kind, "browser_gone", "fewer bytes than declared is the browser's failure");
  assert.equal(fake.transfers[0]!.complete, false, "Briefcase never got a whole (wrong) file");
});

test("refused before anything is asked or read: other sites, no sign-in, a session about to end, uploads off, a page from before, wrong framing", async () => {
  fake = await fakeServices();
  const expectRefusal = async (request: NextRequest, status: number, code: string) => {
    const response = await POST(request);
    assert.equal(response.status, status, code);
    assert.equal((await errorOf(response)).code, code);
  };
  await expectRefusal(upload({ headers: { origin: "https://evil.test", "sec-fetch-site": "cross-site" } }), 403, "cross_site_request");
  await expectRefusal(upload({ session: null }), 401, "signed_out");
  await expectRefusal(upload({ session: session({ ae: Date.now() + 5_000 }) }), 401, "session_refresh_needed");
  await expectRefusal(upload({ name: "  " }), 422, "invalid_name");
  await expectRefusal(upload({ query: { size: null, sha256: null } }), 409, "page_outdated");
  await expectRefusal(upload({ query: { sha256: "abc" } }), 400, "invalid_digest");
  await expectRefusal(upload({ query: { size: "12.5" } }), 400, "invalid_size");
  await expectRefusal(upload({ headers: { "content-length": null, "transfer-encoding": "chunked" } }), 411, "length_required");
  await expectRefusal(upload({ size: 1000, headers: { "content-length": "999" } }), 400, "size_mismatch");
  await expectRefusal(upload({ headers: { "content-encoding": "gzip" } }), 415, "encoded_body");
  applyTestEnv({ BRIEFCASE_API_URL: undefined });
  const off = await POST(upload());
  assert.equal(off.status, 503);
  assert.equal((await errorOf(off)).message, "Uploads are not set up on this DM website yet.");
  assert.deepEqual(fake.routes, [], "nothing reached Accounts or Briefcase");
});

test("sizes: no limit of the website's own up to Briefcase's 5 TiB; an operator's limit is named; an empty file is refused", async () => {
  const settings = { maxBytes: 5 * 1024 ** 4, capped: false };
  const headers = (size: number) => new Headers({ "content-length": String(size) });
  const query = (size: number) => new URLSearchParams({ size: String(size), sha256: "a".repeat(64) });
  const terabyte = 1024 ** 4;
  assert.deepEqual(declaredFile(query(4 * terabyte), headers(4 * terabyte), settings), { size: 4 * terabyte, sha256: "a".repeat(64) }, "4 TiB is fine");
  const over = declaredFile(query(5 * terabyte + 1), headers(5 * terabyte + 1), settings);
  assert.ok(over instanceof UploadFailure && over.status === 413 && /5 TB, the most Silicon Briefcase takes in one file/.test(over.message), String(over instanceof Error ? over.message : over));
  const capped = declaredFile(query(2000), headers(2000), { maxBytes: 1000, capped: true });
  assert.ok(capped instanceof UploadFailure && capped.status === 413 && /1000 bytes, the most this DM website takes/.test(capped.message));
  const empty = declaredFile(query(0), headers(0), settings);
  assert.ok(empty instanceof UploadFailure && empty.code === "file_empty");
  fake = await fakeServices();
  applyTestEnv({ BRIEFCASE_API_URL: fake.url, ACCOUNTS_API_URL: fake.url, UPLOADS_MAX_BYTES: "10" });
  const refused = await POST(upload({ size: 300 }));
  assert.equal(refused.status, 413);
  assert.match((await errorOf(refused)).message, /larger than 10 bytes, the most this DM website takes/);
  assert.deepEqual(fake.routes, []);
});

test("Briefcase not allowing DM yet: said plainly, with what to do instead; nothing reserved, so nothing to cancel; the proof revoked", async () => {
  fake = await fakeServices({ answer: route => (route === "reserve" ? [403, { error: { code: "proof_issuer_not_allowed", message: "dm may not use briefcase.uploads.reserve" } }] : null) });
  const response = await POST(upload());
  assert.equal(response.status, 503);
  const error = await errorOf(response);
  assert.equal(error.code, "briefcase_not_allowed");
  assert.match(error.hint ?? "", /Send a link instead/);
  assert.deepEqual(fake.routes, ["issue", "reserve", "revoke"]);
});

test("Briefcase's limits in words: a full Briefcase at commit is cancelled; too many unfinished uploads waits", async () => {
  fake = await fakeServices({ answer: route => (route === "commit" ? [507, { error: { code: "insufficient_storage", message: "full" } }] : null) });
  const full = await POST(upload());
  assert.equal(full.status, 507);
  assert.equal((await errorOf(full)).code, "briefcase_full");
  assert.ok(fake.routes.includes("cancel"));
  await fake.close();
  fake = await fakeServices({ answer: route => (route === "reserve" ? [409, { error: { code: "delegated_upload_staging_limit_exhausted", message: "x" } }] : null) });
  const busy = await POST(upload());
  assert.equal(busy.status, 429);
  assert.equal((await errorOf(busy)).code, "briefcase_busy");
  assert.equal(busy.headers.get("retry-after"), "30");
});

test("a JSON request that failed on the way goes again once, with a new proof token and the same operation", async () => {
  fake = await fakeServices({ answer: (route, attempt) => (route === "reserve" && attempt === 0 ? [503, { error: { code: "dependency_unavailable", message: "x" } }] : null) });
  const response = await POST(upload());
  assert.equal(response.status, 200);
  assert.deepEqual(fake.routes.filter(route => route === "reserve"), ["reserve", "reserve"]);
  assert.equal(new Set(fake.proofs).size, fake.proofs.length, "a spent proof is never sent again");
});

test("a sign-in Accounts no longer accepts: signed out, nothing reserved", async () => {
  fake = await fakeServices({ answer: route => (route === "issue" ? [400, { error: { code: "invalid_subject_token", message: "The access token was revoked." } }] : null) });
  const response = await POST(upload());
  assert.equal(response.status, 401);
  assert.equal((await errorOf(response)).code, "signed_out");
  assert.deepEqual(fake.routes, ["issue"]);
});

test("names and types: what a person sees, what Briefcase stores, what a download is called", () => {
  assert.equal(cleanFileName("  notes\u0000/2026\\q4.txt "), "notes-2026-q4.txt");
  assert.equal(cleanFileName("invoice‮gnp.exe"), "invoicegnp.exe", "a right-to-left override never disguises an extension");
  assert.equal(cleanFileName("⁦report⁩ ‏final‎.pdf"), "report final.pdf");
  assert.equal(cleanFileName("שלום — 日本語 🎉.txt"), "שלום — 日本語 🎉.txt", "real right-to-left words stay");
  assert.equal(cleanFileName(".."), null);
  assert.equal(cleanFileName(""), null);
  const long = `${"a".repeat(400)}.pdf`;
  assert.equal([...cleanFileName(long)!].length, 232, "room for the stored name's prefix within Briefcase's 255");
  assert.ok(cleanFileName(long)!.endsWith(".pdf"), "the extension stays");
  const stored = storedName("Harbour at dusk.png", new Date("2026-10-17T18:10:14.000Z"), Buffer.from([0, 1, 2, 35, 36, 37]));
  assert.equal(stored, "20261017-181014-012z01-Harbour at dusk.png");
  assert.equal(originalName(stored), "Harbour at dusk.png");
  assert.equal(originalName("plain.png"), "plain.png");
  assert.equal(contentTypeFor("image/png", "a.png"), "image/png");
  assert.equal(contentTypeFor("application/octet-stream", "plan.pdf"), "application/pdf", "an unhelpful type is read from the extension");
  assert.equal(contentTypeFor("", "notes.md"), "text/markdown");
  assert.equal(contentTypeFor("audio/webm;codecs=opus", "note.webm"), "audio/webm;codecs=opus");
  assert.equal(contentTypeFor("not a type", "blob"), "application/octet-stream");
});

test("one account has at most a few uploads at once here: the next is refused before anything is read or asked", async () => {
  fake = await fakeServices();
  const held = Array.from({ length: MAX_UPLOADS_PER_ACCOUNT }, () => claimUploadSlot(OWNER));
  assert.ok(held.every(Boolean), "the slots up to the limit are given");
  assert.equal(claimUploadSlot(OWNER), null, "and no more");
  const other = claimUploadSlot("another-account");
  assert.ok(other, "another account has slots of its own");
  other();
  const refused = await POST(upload());
  assert.equal(refused.status, 429);
  assert.equal(refused.headers.get("retry-after"), "5");
  const error = await errorOf(refused);
  assert.equal(error.code, "too_many_uploads");
  assert.match(error.message, /already have \d+ files uploading/);
  assert.deepEqual(fake.routes, [], "nothing reached Accounts or Briefcase");
  held[0]!();
  held[0]!();
  const slot = claimUploadSlot(OWNER);
  assert.ok(slot, "a slot given back (once, however often it is called) can be taken again");
  assert.equal(claimUploadSlot(OWNER), null);
  for (const give of [...held.slice(1), slot!]) give!();
  const accepted = await POST(upload());
  assert.equal(accepted.status, 200, "every slot came back");
});

test("Briefcase storing a large file past the hold: 202 with an id, then the ask gives the link; the slot and the proof last until then", async () => {
  Object.assign(SAVING_TIMING, { holdMs: 80, waitMs: 5_000 });
  fake = await fakeServices({ answerDelayMs: 600 });
  const size = 2 * MB;
  const posted = await POST(upload({ size }));
  assert.equal(posted.status, 202, JSON.stringify(await posted.clone().json()));
  const pending = (await posted.json()) as { type: string; data: { id: string; name: string; content_type: string; size: number } };
  assert.equal(pending.type, "upload.saving");
  assert.match(pending.data.id, /^[0-9a-f-]{36}$/);
  assert.deepEqual({ name: pending.data.name, size: pending.data.size }, { name: "Quarterly plan.pdf", size });
  assert.equal(posted.headers.get("cache-control"), "no-store");
  assert.deepEqual(fake.revoked, [], "the proof lives on while Briefcase stores the file");
  const held = Array.from({ length: MAX_UPLOADS_PER_ACCOUNT - 1 }, () => claimUploadSlot(OWNER));
  assert.equal(claimUploadSlot(OWNER), null, "the save still holds its upload slot");
  for (const give of held) give!();
  const now = await askSave(saving("GET", pending.data.id, { wait: 0 }));
  assert.equal(now.status, 202, "wait=0 answers where it stands at once");
  const asked = await askSave(saving("GET", pending.data.id));
  assert.equal(asked.status, 200);
  const saved = (await asked.json()) as { type: string; data: { url: string; name: string; size: number } };
  assert.equal(saved.type, "upload");
  assert.match(saved.data.url, /\/api\/v1\/public\/.+Quarterly%20plan\.pdf\?view=inline$/);
  assert.equal(saved.data.size, size);
  await sleep(20);
  assert.deepEqual(fake.routes.filter(route => route !== "refresh"), ["issue", "reserve", "transfer", "commit", "link-access", "revoke"]);
  const slot = claimUploadSlot(OWNER);
  assert.ok(slot, "the slot came back with the save's end");
  slot!();
  const again = await askSave(saving("GET", pending.data.id, { wait: 0 }));
  assert.equal(again.status, 200, "the outcome stays for a while (a repeated ask gets it again)");
});

test("a save another account asks about, or one this server never had, is 404 upload_unknown", async () => {
  Object.assign(SAVING_TIMING, { holdMs: 50 });
  fake = await fakeServices({ answerDelayMs: 400 });
  const posted = await POST(upload({ size: MB }));
  assert.equal(posted.status, 202);
  const { data } = (await posted.json()) as { data: { id: string } };
  const stranger = session({ acct: { uuid: "0f0e0d0c-0b0a-4090-8070-605040302010", kind: "carbon", id: "c:bo", display_name: "Bo", pfp_url: null, custodian: null } });
  for (const request of [saving("GET", data.id, { wait: 0, session: stranger }), saving("DELETE", data.id, { session: stranger }), saving("GET", crypto.randomUUID(), { wait: 0 }), saving("GET", "not-an-id", { wait: 0 })]) {
    const answer = await (request.method === "DELETE" ? cancelSave(request) : askSave(request));
    assert.equal(answer.status, 404);
    assert.equal((await errorOf(answer)).code, "upload_unknown");
  }
  const refused = await cancelSave(saving("DELETE", data.id, { headers: { origin: "https://evil.test", "sec-fetch-site": "cross-site" } }));
  assert.equal(refused.status, 403, "a cancel only from this site's own pages");
  const asked = await askSave(saving("GET", data.id));
  assert.equal(asked.status, 200, "the Carbon's own ask still gets the link");
});

test("cancelled while Briefcase stores it: the connection to Briefcase closes, the reservation is cancelled, nothing is committed", async () => {
  Object.assign(SAVING_TIMING, { holdMs: 50 });
  fake = await fakeServices({ answerDelayMs: 10_000 });
  const posted = await POST(upload({ size: MB }));
  assert.equal(posted.status, 202);
  const { data } = (await posted.json()) as { data: { id: string } };
  const cancelled = await cancelSave(saving("DELETE", data.id));
  assert.equal(cancelled.status, 200);
  assert.deepEqual(((await cancelled.json()) as { data: unknown }).data, { id: data.id, state: "cancelled" });
  assert.equal(fake.transfers[0]!.closedBeforeAnswer, true, "the website stopped waiting for Briefcase's answer");
  await sleep(20);
  assert.ok(fake.routes.includes("cancel"));
  assert.ok(!fake.routes.includes("commit"));
  assert.equal(fake.revoked.length, 1, "the proof is revoked");
  const asked = await askSave(saving("GET", data.id, { wait: 0 }));
  assert.equal(asked.status, 409);
  assert.equal((await errorOf(asked)).code, "upload_cancelled");
});

test("a save nobody asks about (the tab was closed) is abandoned: cancelled, never committed", async () => {
  Object.assign(SAVING_TIMING, { holdMs: 50, abandonMs: 150 });
  fake = await fakeServices({ answerDelayMs: 5_000 });
  const posted = await POST(upload({ size: MB }));
  assert.equal(posted.status, 202);
  const { data } = (await posted.json()) as { data: { id: string } };
  await sleep(500);
  assert.equal(fake.transfers[0]!.closedBeforeAnswer, true);
  assert.ok(fake.routes.includes("cancel"), fake.routes.join(" "));
  assert.ok(!fake.routes.includes("commit"));
  const asked = await askSave(saving("GET", data.id, { wait: 0 }));
  assert.equal(asked.status, 409);
  const error = await errorOf(asked);
  assert.equal(error.code, "upload_abandoned");
  assert.match(error.hint ?? "", /first byte/);
});

test("a page that keeps asking keeps its save: long asks are not abandonment", async () => {
  Object.assign(SAVING_TIMING, { holdMs: 50, abandonMs: 200, waitMs: 5_000 });
  fake = await fakeServices({ answerDelayMs: 700 });
  const posted = await POST(upload({ size: MB }));
  const { data } = (await posted.json()) as { data: { id: string } };
  // One ask that waits longer than abandonMs: the save is not given up while it is asked about.
  const asked = await askSave(saving("GET", data.id));
  assert.equal(asked.status, 200, JSON.stringify(await asked.clone().json()));
  assert.ok(!fake.routes.includes("cancel"));
});

test("a refusal after the hold reaches the ask in the upload's own words", async () => {
  Object.assign(SAVING_TIMING, { holdMs: 50 });
  fake = await fakeServices({ transfer: "refuse-at-end", answerDelayMs: 300 });
  const posted = await POST(upload({ size: MB }));
  assert.equal(posted.status, 202);
  const { data } = (await posted.json()) as { data: { id: string } };
  const asked = await askSave(saving("GET", data.id));
  assert.equal(asked.status, 409);
  assert.equal((await errorOf(asked)).code, "file_changed");
  await sleep(20);
  assert.ok(fake.routes.includes("cancel"));
});

test("names fit Briefcase's 255 bytes with the stored prefix, whatever their script, and never end in a space Briefcase would trim", () => {
  const prefix = storedName("x").length - 1;
  for (const raw of [`${"資料".repeat(150)}.pdf`, `${"नमस्ते".repeat(60)}.docx`, `${"👩🏽‍💻".repeat(40)}.png`, `${"é".repeat(300)}`, `${"a".repeat(400)}.tar.gz`, `${"x".repeat(240)} 　.txt`]) {
    const name = cleanFileName(raw)!;
    assert.ok(name, raw.slice(0, 20));
    assert.ok(Buffer.byteLength(storedName(name)) <= 255, `${Buffer.byteLength(storedName(name))} bytes for ${raw.slice(0, 12)}…`);
    assert.ok(Buffer.byteLength(name) + prefix <= 255);
    assert.equal(name, name.trimEnd());
    assert.ok(!/�/.test(name) && name === name.normalize("NFC"), "no character cut in half");
  }
  assert.ok(cleanFileName(`${"資料".repeat(150)}.pdf`)!.endsWith(".pdf"), "the extension stays");
  // A family emoji is one character of 25 bytes: it is kept whole or left out, never split.
  const family = cleanFileName(`${"a".repeat(220)}👨‍👩‍👧‍👦👨‍👩‍👧‍👦.txt`)!;
  assert.ok(family.endsWith("a.txt") || family.includes("👨‍👩‍👧‍👦"));
  assert.ok(!/‍\.txt$/.test(family), "no joiner left dangling");
  // C1 controls (U+0085 is a space to Briefcase's trim, which would leave a link to a name it does not have).
  assert.equal(cleanFileName("report\u0085"), "report");
  assert.equal(cleanFileName("a\u0080\u009fb.txt"), "ab.txt");
});

test("the browser leaves after its last byte, before the website answers: the save stops and the reservation is cancelled", async () => {
  Object.assign(SAVING_TIMING, { holdMs: 2_000 });
  fake = await fakeServices({ answerDelayMs: 5_000 });
  const leaving = new AbortController();
  const watch = setInterval(() => {
    if (fake!.transfers[0]?.complete) leaving.abort();
  }, 5);
  const response = await POST(upload({ size: MB, signal: leaving.signal }));
  clearInterval(watch);
  assert.equal(response.status, 400);
  assert.equal((await errorOf(response)).code, "upload_interrupted");
  assert.equal(fake.transfers[0]!.closedBeforeAnswer, true);
  assert.ok(fake.routes.includes("cancel"));
  assert.ok(!fake.routes.includes("commit"));
});

test("Briefcase saved the file but its link failed: said as such (never \"not saved\"), and the saved file is not cancelled", async () => {
  fake = await fakeServices({ answer: route => (route === "link-access" ? [503, { error: { code: "dependency_unavailable", message: "x" } }] : null) });
  const response = await POST(upload({ size: MB }));
  assert.equal(response.status, 503);
  const error = await errorOf(response);
  assert.equal(error.code, "briefcase_unavailable");
  assert.match(error.message, /saved the file but did not open a link to it/);
  assert.match(error.hint ?? "", /stays in your Briefcase under apps\/dm/);
  assert.deepEqual(fake.routes.filter(route => route === "link-access").length, 2, "tried twice, with a fresh token each time");
  assert.ok(!fake.routes.includes("cancel"), "a committed file is never cancelled");
});
