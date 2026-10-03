import assert from "node:assert/strict";
import test from "node:test";
import { build } from "esbuild";
import vm from "node:vm";
const bundle = await build({
  entryPoints: [new URL("../src/iam-popup.ts", import.meta.url).pathname],
  bundle: true,
  write: false,
  format: "iife",
  globalName: "popupApi",
});
const profile = "00000000-0000-4000-8000-000000000002";
function fixture(blocked = false, callback?: string) {
  let receive: ((event: any) => void) | undefined;
  const popup = {
    location: { href: "about:blank" },
    closed: false,
    close() {
      this.closed = true;
    },
  };
  const posts: unknown[] = [],
    replacements: string[] = [];
  const window = {
    location: {
      origin: "https://dm.example",
      href: callback || "https://dm.example/",
    },
    opener: {
      postMessage: (value: unknown, origin: string) =>
        posts.push({ value, origin }),
    },
    close: () => popup.close(),
    open: () => (blocked ? null : popup),
    addEventListener: (_name: string, listener: any) => {
      receive = listener;
    },
    removeEventListener: () => {
      receive = undefined;
    },
  };
  const context = vm.createContext({
    window,
    history: {
      replaceState: (_a: unknown, _b: unknown, url: string) =>
        replacements.push(url),
    },
    URL,
    crypto,
    Uint8Array,
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
  });
  vm.runInContext(bundle.outputFiles[0].text, context);
  return {
    api: context.popupApi,
    popup,
    window,
    posts,
    replacements,
    send: (
      data: any,
      origin = window.location.origin,
      source: unknown = popup,
    ) => receive?.({ data, origin, source }),
    listening: () => !!receive,
  };
}
test("popup completion requires exact origin, window, nonce and opaque profile", async () => {
  const f = fixture(),
    controller = new AbortController();
  let nonce = "";
  const pending = f.api.openIamPopup((value: string) => {
    nonce = value;
    return "/auth/login";
  }, controller.signal);
  await Promise.resolve();
  await Promise.resolve();
  const data = {
    type: "silicon:dm-login-complete",
    nonce,
    profile_id: profile,
  };
  f.send(data, "https://attacker.example");
  f.send(data, undefined, {});
  f.send({ ...data, nonce: "b".repeat(64) });
  f.send({ ...data, profile_id: "slt_secret" });
  assert(f.listening());
  assert(!f.popup.closed);
  f.send(data);
  assert.equal(await pending, profile);
  assert(f.popup.closed);
  assert(!f.listening());
});
test("unmount cancels popup and removes completion listener", async () => {
  const f = fixture(),
    controller = new AbortController();
  const pending = f.api.openIamPopup(() => "/auth/login", controller.signal);
  controller.abort();
  await assert.rejects(pending, /cancelled/);
  assert(f.popup.closed);
  assert(!f.listening());
});
test("blocked popup directs user to typed full-page links", async () => {
  const f = fixture(true);
  await assert.rejects(
    f.api.openIamPopup(() => "/auth/login", new AbortController().signal),
    /Carbon or Silicon link/,
  );
});
test("completion removes callback metadata and sends no identity or credential", () => {
  const f = fixture(
    false,
    `https://dm.example/?iam_popup=complete&nonce=${"a".repeat(64)}&profile_id=${profile}`,
  );
  assert.equal(f.api.completeIamPopup(), true);
  assert.deepEqual(JSON.parse(JSON.stringify(f.posts)), [
    {
      value: {
        type: "silicon:dm-login-complete",
        nonce: "a".repeat(64),
        profile_id: profile,
      },
      origin: "https://dm.example",
    },
  ]);
  assert.deepEqual(f.replacements, ["/"]);
  assert(f.popup.closed);
});
test("same-page completion without opener proceeds to normal verified boot", () => {
  const f = fixture(
    false,
    `https://dm.example/?iam_popup=complete&nonce=${"a".repeat(64)}&profile_id=${profile}`,
  );
  (f.window as any).opener = null;
  assert.equal(f.api.completeIamPopup(), false);
  assert.equal(f.posts.length, 0);
  assert.deepEqual(f.replacements, ["/"]);
});
