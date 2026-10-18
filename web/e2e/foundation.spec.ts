/**
 * The chat's foundation, as the fix round of 2026-10-18 left it (each test would have caught a confirmed finding):
 * - ⌘K: what is typed finds its conversation before an action that merely contains the letters, the match Enter runs
 *   is highlighted (and the active descendant), and the arrow keys walk the rows in the order they are drawn;
 * - a conversation opened at another name (`/c/si:dmc-scout`) moves to its own address and stays live;
 * - a draft sent in one tab leaves the other tab's field;
 * - Settings → reach: "Remove Bo?" waits for an answer, and a click elsewhere keeps the keyboard there;
 * - '/' where no search shows goes to the list and puts the keyboard in its search;
 * - someone's activity shows only when the DM service says it is about this conversation;
 * - signing out with messages not sent asks first, then leaves none of the Carbon's words in this browser.
 *
 * Signed in with the stack's sealed cookies (scripts/chat-stack/identities.mjs cookie <who>), each test with data of its
 * own (a group named for the run), and anything shared it touches is answered inside the page, never changed.
 */
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";
import { BASE, sameOrigin, watchErrors } from "./support";

const HELPER = join(process.cwd(), "..", "scripts", "chat-stack", "identities.mjs");
const DM_API = `http://127.0.0.1:${process.env.API_PORT || 4521}`;
const MOD = process.platform === "darwin" ? "Meta" : "Control";
const stamp = () => `${Date.now().toString(36).slice(-5)}${Math.random().toString(36).slice(2, 5)}`;

const helper = (...args: string[]) => execFileSync("node", [HELPER, ...args], { encoding: "utf8" });
const tokens = new Map<string, string>();
const tokenOf = (who: string) => {
  if (!tokens.has(who)) tokens.set(who, helper("token", who, "--raw").trim());
  return tokens.get(who)!;
};

/** A browser signed in to the website as one of the stack's identities (a new sign-in each time). */
async function signedIn(browser: Browser, who: string, viewport = { width: 1440, height: 900 }): Promise<{ context: BrowserContext; page: Page }> {
  const cookie = JSON.parse(helper("cookie", who)) as { name: string; value: string };
  const context = await browser.newContext({ baseURL: BASE, viewport, locale: "en-US", timezoneId: "Asia/Kolkata" });
  await context.addCookies([{ name: cookie.name, value: cookie.value, url: BASE }]);
  return { context, page: await context.newPage() };
}

/** The DM service directly, as one of the stack's identities. */
async function dmApi<T>(who: string, method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${DM_API}${path}`, {
    method,
    headers: { Authorization: `Bearer ${tokenOf(who)}`, "Content-Type": "application/json", "Idempotency-Key": crypto.randomUUID(), "x-dm-contract-version": "4" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`${method} ${path} → ${response.status} ${text}`);
  return (text ? (JSON.parse(text) as { data: T }).data : null) as T;
}

/**
 * A group of Ada's with Bo in it, named for the run, and a way for Ada to leave it afterwards (a group keeps an owner,
 * so Bo becomes one first and keeps it, as the other specs leave theirs).
 */
async function ownGroup(name: string): Promise<{ id: string; leave: () => Promise<void> }> {
  const group = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name, member_ids: ["c:dmc-bo"] } });
  const id = encodeURIComponent(group.id);
  await dmApi("bo", "POST", `/api/v1/groups/${id}/accept`);
  const leave = async () => {
    await dmApi("ada", "POST", `/api/v1/groups/${id}/owners`, { type: "add_group_owners", data: { member_ids: ["c:dmc-bo"] } }).catch(() => undefined);
    await dmApi("ada", "POST", `/api/v1/groups/${id}/leave`).catch(() => undefined);
  };
  return { id: group.id, leave };
}

const field = (page: Page) => page.getByRole("textbox", { name: /^Message / });

test("⌘K: a conversation named by what is typed comes first, highlighted, and the arrows walk the rows as drawn", async ({ browser }) => {
  const name = `Keyboard ${stamp()}`;
  const group = await ownGroup(name);
  const { context, page } = await signedIn(browser, "ada");
  const errors = watchErrors(page);
  try {
    await page.goto("/c");
    await page.locator("main").click({ position: { x: 8, y: 300 } });
    await page.keyboard.press(`${MOD}+K`);
    const input = page.getByRole("combobox", { name: "Search conversations and actions" });
    await expect(input).toBeFocused();
    // With nothing typed: what the keyboard walks is what is drawn, top to bottom.
    const drawn = await page.locator("[role='option']").evaluateAll(nodes => nodes.slice(0, 10).map(node => node.id));
    const walked: string[] = [];
    for (let step = 0; step < drawn.length; step++) {
      await page.keyboard.press("ArrowDown");
      walked.push((await input.getAttribute("aria-activedescendant")) ?? "");
    }
    expect(walked).toEqual(drawn);
    // "keyboard" is in an action's name too (Keyboard shortcuts): the conversation comes first, and is what Enter runs.
    await input.fill("keyboard");
    const first = page.locator("[role='option']").first();
    await expect(first).toContainText(name);
    await expect(first).toHaveAttribute("aria-selected", "true");
    await expect(input).toHaveAttribute("aria-activedescendant", (await first.getAttribute("id"))!);
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(new RegExp(`/c/${group.id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`));
    // Nothing found is said, not only drawn.
    await page.locator("main").click({ position: { x: 8, y: 300 } });
    await page.keyboard.press(`${MOD}+K`);
    await page.keyboard.type(`zz${stamp()}`);
    await expect(page.getByRole("status").filter({ hasText: "No conversation or action matches" })).toHaveCount(1);
    expect(errors).toEqual([]);
  } finally {
    await context.close();
    await group.leave();
  }
});

test("a conversation opened at another name moves to its own address, keeps the message link, and stays live", async ({ browser }) => {
  const opened = await dmApi<{ "message-id": string }>("scout", "POST", "/api/v1/conversations/c:dmc-ada/messages", { type: "message.create", data: { message: `Alias check ${stamp()}` } });
  const { context, page } = await signedIn(browser, "ada");
  try {
    await page.goto(`/c/si:dmc-scout#${opened["message-id"]}`);
    await expect(page).toHaveURL(new RegExp(`/c/c:dmc-ada::si:dmc-scout#${opened["message-id"]}$`));
    const incoming = `Live after the move ${stamp()}`;
    await expect(page.locator(`article[data-message-code="${opened["message-id"]}"]`)).toBeVisible();
    await dmApi("scout", "POST", "/api/v1/conversations/c:dmc-ada/messages", { type: "message.create", data: { message: incoming } });
    await expect(page.locator("article[data-message-row]", { hasText: incoming })).toBeVisible({ timeout: 30_000 });
    const mine = `Sent after the move ${stamp()}`;
    await field(page).fill(mine);
    await field(page).press("Enter");
    const row = page.locator("article[data-message-row]", { hasText: mine });
    await expect(row).toHaveAttribute("data-message-code", /.+/);
    await page.waitForTimeout(3000);
    await expect(row).toBeVisible();
  } finally {
    await context.close();
  }
});

test("a draft sent in one tab leaves the other tab's field", async ({ browser }) => {
  const group = await ownGroup(`Tabs ${stamp()}`);
  const { context, page: first } = await signedIn(browser, "ada");
  try {
    await first.goto(`/c/${group.id}`);
    const words = `One draft, two tabs ${stamp()}`;
    await field(first).fill(words);
    await first.waitForTimeout(2200);
    const second = await context.newPage();
    await second.goto(`/c/${group.id}`);
    await expect(field(second)).toHaveValue(words);
    await first.bringToFront();
    await field(first).press("Enter");
    await expect(first.locator("article[data-message-row]", { hasText: words })).toHaveAttribute("data-message-code", /.+/);
    await expect(field(second)).toHaveValue("");
  } finally {
    await context.close();
    await group.leave();
  }
});

test("Settings → reach: the question waits for an answer, and a click elsewhere keeps the keyboard there", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "ada");
  const writes: string[] = [];
  try {
    // Ada's Silicons' lists are shared by every run: a change is answered here and never reaches the service.
    await page.route(/\/api\/api\/v1\/allow-list/, route => {
      if (route.request().method() === "GET") return route.continue();
      writes.push(`${route.request().method()} ${route.request().postData()}`);
      return route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ type: "error", data: { error: { code: "busy", message: "Held by the test." } } }) });
    });
    await page.goto("/settings#reach");
    const remove = page.getByRole("button", { name: /^Remove c:dmc-bo$/ }).first();
    await remove.click();
    const question = page.getByText(/^Remove Bo\?$/).first();
    await expect(question).toBeVisible();
    await page.waitForTimeout(7000);
    await expect(question).toBeVisible();
    // A click into another field closes the question and keeps the keyboard there: spaces typed never answer it.
    const report = page.getByRole("textbox", { name: /What happened/ });
    await report.click();
    await page.keyboard.type("a b c d e", { delay: 80 });
    await expect(report).toHaveValue("a b c d e");
    await expect(report).toBeFocused();
    expect(writes).toEqual([]);
    await report.fill("");
    // Esc gives the keyboard back to Remove (once the question has taken it).
    await remove.focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("button", { name: "Remove", exact: true })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(remove).toBeFocused();
  } finally {
    await context.close();
  }
});

test("'/' where no search shows goes to the list and puts the keyboard in its search", async ({ browser }) => {
  const group = await ownGroup(`Slash ${stamp()}`);
  const { context, page } = await signedIn(browser, "ada", { width: 720, height: 600 });
  try {
    await page.goto(`/c/${group.id}`);
    await expect(field(page)).toBeVisible();
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await page.keyboard.press("/");
    await expect(page).toHaveURL(/\/c$/);
    await expect(page.getByRole("searchbox", { name: "Search conversations" })).toBeFocused();
  } finally {
    await context.close();
    await group.leave();
  }
});

test("someone's activity shows only when the DM service says it is about this conversation", async ({ browser }) => {
  const direct = await dmApi<{ id: string }>("ada", "POST", "/api/v1/conversations", { type: "create_conversation", data: { participant_ids: ["c:dmc-cy"] } });
  const typing = (route: import("@playwright/test").Route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ type: "presence", data: { member_id: "c:dmc-cy", availability: "online", activity: "typing", last_seen_at: new Date().toISOString() } }) });
  // A service without activity per conversation (the contracts held without it): Cy may be typing to anyone, so
  // nothing says she types here.
  const plain = await signedIn(browser, "ada");
  await plain.page.route(/\/api\/api\/v1\/contracts$/, async route => {
    const response = await route.fetch();
    const body = (await response.json()) as { data: { features?: Record<string, unknown> } };
    if (body.data.features) delete body.data.features.presence_activity;
    await route.fulfill({ response, json: body });
  });
  await plain.page.route(/\/api\/api\/v1\/presence\/c(%3A|:)dmc-cy/, typing);
  await plain.page.goto(`/c/${direct.id}`);
  await expect(plain.page.getByRole("heading", { level: 1 })).toBeVisible();
  await plain.page.waitForTimeout(3000);
  await expect(plain.page.getByText(/typing…/)).toHaveCount(0);
  await plain.context.close();
  // A service that says it: the website asks about this conversation, and shows it.
  const scoped = await signedIn(browser, "ada");
  const asked: string[] = [];
  await scoped.page.route(/\/api\/api\/v1\/contracts$/, async route => {
    const response = await route.fetch();
    const body = (await response.json()) as { data: { features?: Record<string, unknown> } };
    body.data.features = { ...(body.data.features ?? {}), presence_activity: { scope: "conversation", renew_field: "conversation_id", query: "conversation" } };
    await route.fulfill({ response, json: body });
  });
  await scoped.page.route(/\/api\/api\/v1\/presence\/c(%3A|:)dmc-cy/, route => {
    asked.push(new URL(route.request().url()).searchParams.get("conversation") ?? "");
    return typing(route);
  });
  await scoped.page.goto(`/c/${direct.id}`);
  await expect(scoped.page.getByText(/typing…/).first()).toBeVisible({ timeout: 30_000 });
  expect(asked).toContain(direct.id);
  await scoped.context.close();
});

test("signing out with a message not sent asks first, then leaves none of the Carbon's words in this browser", async ({ browser }) => {
  const group = await ownGroup(`Sign out ${stamp()}`);
  const { context, page } = await signedIn(browser, "ada");
  try {
    await page.goto(`/c/${group.id}`);
    // The send cannot go (the connection fails), so it waits in the outbox; a draft is typed after it.
    await page.route(/\/messages$/, route => (route.request().method() === "POST" ? route.abort("failed") : route.continue()));
    await field(page).fill(`Never sent ${stamp()}`);
    await field(page).press("Enter");
    await field(page).fill("A draft left behind");
    await page.waitForTimeout(500);
    const kept = () => page.evaluate(() => Object.keys(localStorage).filter(key => /^dm:(outbox|draft|tray):v1:/.test(key)));
    expect((await kept()).length).toBeGreaterThan(0);
    const signOut = async () => {
      await page.getByRole("button", { name: /^Account menu, / }).click();
      await page.getByRole("menuitem", { name: "Sign out" }).click();
    };
    await signOut();
    const dialog = page.getByRole("dialog", { name: "Sign out with messages not sent?" });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole("button", { name: "Stay signed in" })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(page).toHaveURL(new RegExp(`/c/`));
    await signOut();
    await dialog.getByRole("button", { name: "Sign out anyway" }).click();
    await expect(page).toHaveURL(/\/sign-in\?signed_out=1/);
    expect(await kept()).toEqual([]);
  } finally {
    await context.close();
    await group.leave();
  }
});

test("the sign-in page answers any error code, the ones every object has included", async ({ request }) => {
  for (const code of ["__proto__", "constructor", "toString", "hasOwnProperty"]) {
    const response = await request.get(`/sign-in?error=${code}`, { headers: sameOrigin() });
    expect(response.status(), code).toBe(200);
    expect(await response.text(), code).toContain("The sign-in did not finish. Start it again.");
  }
});
