/**
 * POST /api/uploads against fake Silicon Accounts and Briefcase answers (fetch replaced): one User verification proof
 * per upload, a proof token never sent twice (a fresh one for every Briefcase request), the bytes with the capability
 * alone and their exact length, the public link back, the proof revoked and the temp file gone whatever happened; the
 * refusals in words a Carbon reads.
 */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { afterEach, beforeEach, test } from "node:test";
import { NextRequest } from "next/server";
import { POST } from "../app/api/uploads/route";
import { claimUploadSlot, cleanFileName, contentTypeFor, MAX_UPLOADS_PER_ACCOUNT, originalName, storedName } from "../lib/server/briefcase";
import { sealSession, type StoredSession } from "../lib/server/session";
import { applyTestEnv, json, mockFetch } from "./env";

const ACCOUNTS = "http://127.0.0.1:9589";
const API = "https://api.briefcase.test";
const OWNER = "4143123f-b494-481c-adbf-c14b14cfccc0";
const UPLOAD = "6d1f3c1e-6f2b-4b8e-9a51-2a8d7d0b6f11";
const ENTRY = "a1e3c9d2-1b7f-4c55-8d4e-9f0a1b2c3d4e";
const ORIGIN = "http://127.0.0.1:4260";
const later = () => new Date(Date.now() + 600_000).toISOString();

const session = (overrides: Partial<StoredSession> = {}): StoredSession => ({
  v: 1, at: "eyJ.live.sig", rt: "sar_live", ae: Date.now() + 20 * 60_000, re: Date.now() + 86_400_000, scope: "profile",
  acct: { uuid: OWNER, kind: "carbon", id: "c:ada", display_name: "Ada", pfp_url: null, custodian: null }, ...overrides,
});

let restore: () => void = () => undefined;
beforeEach(() => applyTestEnv({ BRIEFCASE_API_URL: API, UPLOADS_MAX_BYTES: undefined }));
afterEach(() => {
  restore();
  restore = () => undefined;
  applyTestEnv({ BRIEFCASE_API_URL: undefined, UPLOADS_MAX_BYTES: undefined });
});

const BYTES = new TextEncoder().encode("%PDF-1.4 quarterly plan");

function upload(options: { name?: string; body?: Uint8Array<ArrayBuffer>; headers?: Record<string, string>; session?: StoredSession | null } = {}) {
  const name = options.name ?? "Quarterly plan.pdf";
  const value = options.session === undefined ? session() : options.session;
  return new NextRequest(`${ORIGIN}/api/uploads?${new URLSearchParams({ name })}`, {
    method: "POST",
    body: options.body ?? BYTES,
    headers: { ...(value ? { cookie: `webkit_session=${sealSession(value)}` } : {}), origin: ORIGIN, "sec-fetch-site": "same-origin", "content-type": "application/pdf", ...options.headers },
  });
}

const leftovers = () => new Set(readdirSync(tmpdir()).filter(name => name.startsWith("dm-upload-")));

interface Fake {
  /** Proof tokens Briefcase received, in order. */
  proofs: string[];
  /** Briefcase routes called, in order. */
  routes: string[];
  revoked: string[];
  refreshKeys: string[];
}

/** Accounts and Briefcase as they answer a good upload; `override` changes one answer. */
function fakeServices(override: (route: string, attempt: number) => Response | null = () => null): Fake & { restore: () => void; calls: Array<{ url: string; init: RequestInit }> } {
  const fake: Fake = { proofs: [], routes: [], revoked: [], refreshKeys: [] };
  let refreshes = 0;
  let stored = "";
  const attempts = new Map<string, number>();
  const mock = mockFetch(async (url, init) => {
    const headers = new Headers(init.headers);
    const body = typeof init.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null;
    if (url.startsWith(ACCOUNTS)) {
      assert.equal(headers.get("authorization"), `Basic ${Buffer.from("webkit:sa_app_webkit_testtesttesttesttesttest").toString("base64")}`, "Accounts calls carry this app's credentials");
      if (url === `${ACCOUNTS}/v1/proofs/user-verification`) {
        assert.equal(body?.subject_token, "eyJ.live.sig");
        assert.equal(body?.receiving_app, "briefcase");
        assert.deepEqual(body?.scopes, ["briefcase.uploads.reserve", "briefcase.uploads.commit", "briefcase.uploads.status", "briefcase.uploads.cancel", "briefcase.link_access.update"]);
        assert.equal(body?.access_ttl_seconds, 600);
        assert.ok(headers.get("idempotency-key"), "issuing carries an Idempotency-Key");
        return override("issue", 0) ?? json(201, { proof_id: "p1", kind: "user_verification", proof_token: "sap_1", proof_refresh_token: "sapr_1", expires_at: later(), user: { uuid: OWNER, id: "c:ada" } });
      }
      if (url === `${ACCOUNTS}/v1/proofs/refresh`) {
        refreshes += 1;
        assert.equal(body?.proof_refresh_token, `sapr_${refreshes}`, "always the newest refresh token");
        fake.refreshKeys.push(headers.get("idempotency-key") ?? "");
        return json(200, { proof_id: "p1", proof_token: `sap_${refreshes + 1}`, proof_refresh_token: `sapr_${refreshes + 1}`, expires_at: later() });
      }
      if (url === `${ACCOUNTS}/v1/proofs/revoke`) {
        fake.revoked.push(String(body?.proof_id));
        return new Response(null, { status: 204 });
      }
    }
    if (url.startsWith(`${API}/api/v1/obo/`)) {
      const route = url.slice(`${API}/api/v1/obo/`.length);
      const name = route.endsWith("/content") ? "transfer" : route;
      const attempt = attempts.get(name) ?? 0;
      attempts.set(name, attempt + 1);
      fake.routes.push(name);
      if (name === "transfer") {
        assert.equal(url, `${API}/api/v1/obo/uploads/${UPLOAD}/content`);
        assert.equal(init.method, "PUT");
        assert.equal(headers.get("authorization"), null, "the bytes carry no account credential");
        assert.equal(headers.get("x-briefcase-upload-capability"), "bcu_cap");
        assert.ok(init.body instanceof Blob, "a file-backed Blob: an exact Content-Length, never chunked");
        assert.deepEqual(new Uint8Array(await (init.body as Blob).arrayBuffer()), BYTES);
        return override(name, attempt) ?? json(200, { operation_id: "op", upload_id: UPLOAD, state: "staged", expires_at: later(), published_entry_id: null });
      }
      const proof = headers.get("authorization") ?? "";
      assert.match(proof, /^Proof sap_\d+$/);
      fake.proofs.push(proof);
      const replaced = override(name, attempt);
      if (replaced) return replaced;
      if (name === "uploads/reserve") {
        stored = String(body?.name);
        assert.equal(body?.parent_path, "", "apps/dm, the app's own folder");
        assert.equal(body?.content_type, "application/pdf");
        assert.equal(body?.size, BYTES.length);
        assert.equal(body?.sha256, createHash("sha256").update(BYTES).digest("hex"));
        assert.match(stored, /^\d{8}-\d{6}-[0-9a-z]{6}-Quarterly plan\.pdf$/, "a name of its own: Briefcase keeps one file per path");
        return json(200, { operation_id: body?.operation_id, upload_id: UPLOAD, state: "reserved", expires_at: later(), published_entry_id: null, capability: "bcu_cap" });
      }
      if (name === "uploads/commit") return json(200, { operation_id: body?.operation_id, upload_id: UPLOAD, state: "committed", expires_at: later(), published_entry_id: ENTRY });
      if (name === "link-access") {
        assert.equal(body?.entry_id, ENTRY);
        assert.equal(body?.enabled, true);
        return json(200, { url: `https://briefcase.test/c:ada/apps/webkit/${encodeURIComponent(stored)}`, enabled: true, effective: true, inherited_from: null, can_manage: true, expires_at: null });
      }
      if (name === "uploads/status") return json(200, { operation_id: body?.operation_id, upload_id: UPLOAD, state: "reserved", expires_at: later(), published_entry_id: null });
      if (name === "uploads/cancel") return json(200, { operation_id: body?.operation_id, upload_id: UPLOAD, state: "cancelled", expires_at: later(), published_entry_id: null });
    }
    throw new Error(`unexpected call ${init.method ?? "GET"} ${url}`);
  });
  return { ...fake, get proofs() { return fake.proofs; }, get routes() { return fake.routes; }, get revoked() { return fake.revoked; }, get refreshKeys() { return fake.refreshKeys; }, restore: mock.restore, calls: mock.calls };
}

const errorOf = async (response: Response) => ((await response.json()) as { error: { code: string; message: string; hint?: string } }).error;

test("an upload: reserve → transfer → commit → link, a fresh proof token each time, the link back, nothing left behind", async () => {
  const before = leftovers();
  const fake = fakeServices();
  restore = fake.restore;
  const response = await POST(upload());
  assert.equal(response.status, 200, JSON.stringify(await response.clone().json()));
  const answer = (await response.json()) as { type: string; data: { url: string; name: string; content_type: string; size: number } };
  assert.equal(answer.type, "upload");
  assert.equal(answer.data.name, "Quarterly plan.pdf");
  assert.equal(answer.data.content_type, "application/pdf");
  assert.equal(answer.data.size, BYTES.length);
  assert.match(answer.data.url, new RegExp(`^${API}/api/v1/public/${OWNER}/apps/webkit/\\d{8}-\\d{6}-[0-9a-z]{6}-Quarterly%20plan\\.pdf\\?view=inline$`), "the owner by uuid, the public inline view");
  assert.deepEqual(fake.routes, ["uploads/reserve", "transfer", "uploads/commit", "link-access"]);
  assert.deepEqual(fake.proofs, ["Proof sap_1", "Proof sap_2", "Proof sap_3"], "never the same proof token twice");
  assert.equal(new Set(fake.refreshKeys).size, fake.refreshKeys.length, "each refresh has its own Idempotency-Key");
  assert.deepEqual(fake.revoked, ["p1"], "the proof ends with the upload");
  assert.equal(response.headers.get("cache-control"), "no-store");
  assert.deepEqual([...leftovers()].filter(name => !before.has(name)), [], "the temp file is gone");
});

test("a request that failed on the way goes again once, with a new proof token and the same operation", async () => {
  const operations: unknown[] = [];
  const fake = fakeServices((route, attempt) => (route === "uploads/reserve" && attempt === 0 ? json(503, { error: { code: "dependency_unavailable", message: "x" } }) : null));
  restore = fake.restore;
  const original = globalThis.fetch;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    if (String(input).endsWith("/uploads/reserve") && typeof init?.body === "string") operations.push((JSON.parse(init.body) as { operation_id: string }).operation_id);
    return original(input, init);
  }) as typeof fetch;
  try {
    const response = await POST(upload());
    assert.equal(response.status, 200);
  } finally {
    globalThis.fetch = original;
  }
  assert.deepEqual(fake.routes.slice(0, 2), ["uploads/reserve", "uploads/reserve"]);
  assert.equal(fake.proofs[0] === fake.proofs[1], false, "a spent proof is never sent again");
  assert.equal(operations[0], operations[1], "the same logical operation, so Briefcase answers it once");
});

test("a transfer that broke is reconciled through its status and a new reservation, then finished", async () => {
  const fake = fakeServices((route, attempt) => (route === "transfer" && attempt === 0 ? json(500, { error: { code: "internal", message: "x" } }) : null));
  restore = fake.restore;
  const response = await POST(upload());
  assert.equal(response.status, 200);
  assert.deepEqual(fake.routes, ["uploads/reserve", "transfer", "uploads/status", "uploads/reserve", "transfer", "uploads/commit", "link-access"]);
  assert.equal(new Set(fake.proofs).size, fake.proofs.length);
});

test("Briefcase not allowing DM yet: said plainly, with what to do instead; the proof revoked", async () => {
  const fake = fakeServices(route => (route === "uploads/reserve" ? json(403, { error: { code: "proof_issuer_not_allowed", message: "dm may not use briefcase.uploads.reserve" } }) : null));
  restore = fake.restore;
  const response = await POST(upload());
  assert.equal(response.status, 503);
  const error = await errorOf(response);
  assert.equal(error.code, "briefcase_not_allowed");
  assert.equal(error.message, "Silicon Briefcase does not accept files from DM yet.");
  assert.match(error.hint ?? "", /Send a link instead/);
  assert.deepEqual(fake.routes, ["uploads/reserve"], "no cancel: nothing was reserved");
  assert.deepEqual(fake.revoked, ["p1"]);
});

test("a full Briefcase at commit: the reservation is cancelled with a proof of its own", async () => {
  const fake = fakeServices(route => (route === "uploads/commit" ? json(507, { error: { code: "insufficient_storage", message: "full" } }) : null));
  restore = fake.restore;
  const response = await POST(upload());
  assert.equal(response.status, 507);
  assert.equal((await errorOf(response)).code, "briefcase_full");
  assert.deepEqual(fake.routes, ["uploads/reserve", "transfer", "uploads/commit", "uploads/cancel"]);
  assert.equal(new Set(fake.proofs).size, fake.proofs.length, "the cancel has its own fresh token");
  assert.deepEqual(fake.revoked, ["p1"]);
});

test("refused before Briefcase is asked: other sites, no sign-in, a session about to end, uploads off, no name", async () => {
  const fake = fakeServices();
  restore = fake.restore;
  const crossSite = await POST(upload({ headers: { origin: "https://evil.test", "sec-fetch-site": "cross-site" } }));
  assert.equal(crossSite.status, 403);
  assert.equal((await errorOf(crossSite)).code, "cross_site_request");
  const signedOut = await POST(upload({ session: null }));
  assert.equal(signedOut.status, 401);
  assert.equal((await errorOf(signedOut)).code, "signed_out");
  const ending = await POST(upload({ session: session({ ae: Date.now() + 5_000 }) }));
  assert.equal(ending.status, 401);
  assert.equal((await errorOf(ending)).code, "session_refresh_needed", "the browser renews its session and sends again; this route never rotates the refresh token");
  const unnamed = await POST(upload({ name: "  " }));
  assert.equal(unnamed.status, 422);
  assert.equal((await errorOf(unnamed)).code, "invalid_name");
  applyTestEnv({ BRIEFCASE_API_URL: undefined });
  const off = await POST(upload());
  assert.equal(off.status, 503);
  assert.equal((await errorOf(off)).message, "Uploads are not set up on this DM website yet.");
  assert.equal(fake.calls.length, 0, "nothing reached Accounts or Briefcase");
});

test("too large (the cap names its size), empty, and a sign-in Accounts no longer accepts", async () => {
  applyTestEnv({ BRIEFCASE_API_URL: API, UPLOADS_MAX_BYTES: "10" });
  let fake = fakeServices();
  restore = fake.restore;
  const large = await POST(upload());
  assert.equal(large.status, 413);
  const tooLarge = await errorOf(large);
  assert.equal(tooLarge.code, "file_too_large");
  assert.match(tooLarge.message, /larger than 10 bytes/);
  assert.deepEqual(fake.routes, [], "Briefcase never saw it");
  fake.restore();
  applyTestEnv({ BRIEFCASE_API_URL: API, UPLOADS_MAX_BYTES: undefined });
  fake = fakeServices();
  restore = fake.restore;
  const empty = await POST(upload({ body: new Uint8Array() }));
  assert.equal(empty.status, 422);
  assert.equal((await errorOf(empty)).code, "file_empty");
  assert.deepEqual(fake.revoked, ["p1"], "the proof issued for it ends too");
  fake.restore();
  fake = fakeServices(route => (route === "issue" ? json(400, { error: { code: "invalid_subject_token", message: "The access token was revoked." } }) : null));
  restore = fake.restore;
  const ended = await POST(upload());
  assert.equal(ended.status, 401);
  assert.equal((await errorOf(ended)).code, "signed_out");
});

test("names and types: what a person sees, what Briefcase stores, what a download is called", () => {
  assert.equal(cleanFileName("  notes\u0000/2026\\q4.txt "), "notes-2026-q4.txt");
  assert.equal(cleanFileName("invoice\u202Egnp.exe"), "invoicegnp.exe", "a right-to-left override never disguises an extension");
  assert.equal(cleanFileName("\u2066report\u2069 \u200Ffinal\u200E.pdf"), "report final.pdf");
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
  const fake = fakeServices();
  restore = fake.restore;
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
  assert.equal(fake.calls.length, 0, "nothing reached Accounts or Briefcase");
  held[0]!();
  held[0]!();
  const slot = claimUploadSlot(OWNER);
  assert.ok(slot, "a slot given back (once, however often it is called) can be taken again");
  assert.equal(claimUploadSlot(OWNER), null);
  for (const give of [...held.slice(1), slot!]) give!();
  const accepted = await POST(upload());
  assert.equal(accepted.status, 200, "every slot came back");
});
