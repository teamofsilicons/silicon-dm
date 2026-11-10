/** Run with E2E_TING=1 API_PORT=4747: real Accounts/DM plus a page-owned Ting
 * transport. This exercises CSP, cross-origin requests and runtime teardown. */
import { expect, test, type WebSocketRoute } from "@playwright/test";
import { BASE, statePath, sessionOf } from "./support";

test("Ting receiver connects, wakes DM recovery, and refuses a different account", async ({ browser }) => {
  test.skip(process.env.E2E_TING !== "1", "requires the isolated Ting discovery adapter");
  const context = await browser.newContext({ baseURL: BASE, storageState: statePath("carbon") });
  try {
    const identity = (await sessionOf(context)).account!;
    expect(identity?.uuid).toBeTruthy();
    const page = await context.newPage();
    let attached = identity.uuid;
    let sockets = 0, checks = 0, syncs = 0;
    let live!: WebSocketRoute;
    const commands: Record<string, unknown>[] = [];
    const errors: string[] = [];
    page.on("pageerror", error => errors.push(error.message));
    page.on("request", request => { if (request.url().includes("/api/v1/sync?")) syncs++; });
    await page.route("http://127.0.0.1:4748/v1/me", route => {
      checks++;
      return route.fulfill({ contentType: "application/json", headers: { "Access-Control-Allow-Origin": BASE, "Access-Control-Allow-Credentials": "true" }, body: JSON.stringify({ authenticated: true, uuid: identity.uuid, kind: identity.kind }) });
    });
    await page.routeWebSocket("ws://127.0.0.1:4748/v1/ws?protocol=v1", socket => {
      live = socket;
      const session = `browser-receiver-${++sockets}`;
      socket.onMessage(raw => {
        const frame = JSON.parse(String(raw)); commands.push(frame);
        if (frame.op === "register") socket.send(JSON.stringify({ op: "registered", request_id: frame.request_id, session_id: session, for: attached, app_ids: [] }));
        if (frame.op === "watch_inbox") socket.send(JSON.stringify({ op: "watching_inbox", request_id: frame.request_id, session_id: session }));
      });
      socket.send(JSON.stringify({ op: "ready", protocol: "v1", session_id: session, receiver_id: session }));
    });
    const response = await page.goto("/settings");
    expect(response?.headers()["content-security-policy"]).toContain("connect-src 'self' http://127.0.0.1:4748 ws://127.0.0.1:4748");
    await expect(page.getByText("Connected through Ting.", { exact: false })).toBeVisible();
    await expect.poll(() => syncs).toBeGreaterThan(0);
    const before = syncs;
    live.send(JSON.stringify({ op: "silent_digest", session_id: `browser-receiver-${sockets}`, tings: [{ type: "dm.syncChanged", for: identity.uuid }] }));
    await expect.poll(() => syncs, { timeout: 5_000 }).toBeGreaterThan(before);
    attached = "another-account";
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.getByRole("link", { name: "Sign in to Ting" })).toBeVisible();
    await page.setViewportSize({ width: 390, height: 844 });
    await page.getByRole("link", { name: "Sign in to Ting" }).scrollIntoViewIfNeeded();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: test.info().outputPath("ting-signin-phone.png") });
    attached = identity.uuid;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.getByText("Connected through Ting.", { exact: false })).toBeVisible();
    expect(checks).toBeGreaterThanOrEqual(3);
    expect(sockets).toBeGreaterThanOrEqual(3);
    expect(commands.every(frame => ["register", "watch_inbox"].includes(String(frame.op)) && !("session_token" in frame))).toBe(true);
    expect(errors).toEqual([]);
  } finally { await context.close(); }
});
