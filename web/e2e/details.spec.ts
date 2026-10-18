/**
 * The details area against the local chat stack (scripts/chat-stack/up.sh): a group managed from the details panel
 * (rename, add and remove members, make an owner, leave), an invitation accepted from the Invitations dialog, a
 * Carbon who looks after no Silicon starting a conversation with one and inviting one to a group (and a refusal in the
 * service's words first), Settings and a Silicon's details with no reach section (an old `/settings#reach` link lands
 * at the top), a rebound shortcut that survives a reload (and is reset), focus and Esc around the dialogs and the
 * drawer, a custodian's read-only view, a report's link checked before it is sent, and a details panel that never
 * scrolls sideways on a phone. Every run makes its own groups and asserts only on them: other agents use the same
 * stack.
 *
 * Signed in with the stack's sealed cookies (scripts/chat-stack/identities.mjs cookie <who>), never with email codes.
 */
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";
import { BASE, sameOrigin, watchErrors } from "./support";

const HELPER = join(process.cwd(), "..", "scripts", "chat-stack", "identities.mjs");
const MOD = process.platform === "darwin" ? "Meta" : "Control";
const stamp = () => `${Date.now().toString(36).slice(-4)}${Math.random().toString(36).slice(2, 5)}`;

/** A browser signed in to the website as one of the stack's identities (ada, bo, cy, di). */
async function signedIn(browser: Browser, who: string, viewport = { width: 1440, height: 900 }) {
  const { account, ...cookie } = JSON.parse(execFileSync("node", [HELPER, "cookie", who], { encoding: "utf8" })) as { account: { id: string }; name: string; value: string; domain: string; path: string };
  const context = await browser.newContext({ baseURL: BASE, viewport, locale: "en-US", timezoneId: "Asia/Kolkata" });
  await context.addCookies([cookie as never]);
  const page = await context.newPage();
  return { context, page, id: account.id };
}

/** The details panel of the open conversation, opened with ⌘. when it is not showing yet. */
async function openDetails(page: Page) {
  const panel = page.locator("[data-details-panel]");
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  // Wide screens remember the panel open: give it a moment to show before toggling it.
  const shown = await panel.waitFor({ state: "visible", timeout: 1500 }).then(() => true, () => false);
  if (!shown) {
    await page.locator("main").click({ position: { x: 8, y: 300 } });
    await page.keyboard.press(`${MOD}+Period`);
  }
  await expect(panel).toBeVisible();
  return panel;
}

/** One call to the DM service through the website's proxy, as the signed-in account (setup and clean-up). */
async function call(context: BrowserContext, method: string, path: string, body?: unknown) {
  const response = await context.request.fetch(`/api${path}`, {
    method,
    headers: sameOrigin({ "x-dm-contract-version": "4", ...(body ? { "Content-Type": "application/json", "Idempotency-Key": crypto.randomUUID() } : {}) }),
    data: body ? JSON.stringify(body) : undefined,
  });
  return { status: response.status(), json: (await response.json().catch(() => null)) as { data?: Record<string, unknown> } | null };
}

/** Everything inside `selector` that reaches past its right edge, or past the window's (a sideways scroll). */
async function wideIn(page: Page, selector: string) {
  return page.evaluate(selector => {
    const root = document.querySelector<HTMLElement>(selector)!;
    const edge = Math.min(root.getBoundingClientRect().right, window.innerWidth) + 0.5;
    const wide: string[] = [];
    for (const node of [root, ...root.querySelectorAll<HTMLElement>("*")]) {
      const box = node.getBoundingClientRect();
      if (box.width && box.right > edge) wide.push(`${node.tagName.toLowerCase()}.${String(node.className).slice(0, 40)}`);
      const style = getComputedStyle(node);
      if (/(auto|scroll)/.test(style.overflowX) && node.scrollWidth > node.clientWidth + 1) wide.push(`${node.tagName.toLowerCase()} scrolls sideways`);
    }
    return wide.slice(0, 5);
  }, selector);
}

/** Waits until the settings column stops growing (sections that load their data), so a click lands where it aims. */
async function settingsSettled(page: Page) {
  let last = -1;
  await expect.poll(async () => {
    const height = await page.evaluate(() => document.getElementById("settings-scroll")?.scrollHeight ?? 0);
    const same = height === last;
    last = height;
    return same;
  }, { intervals: [300], timeout: 10_000 }).toBe(true);
}

async function createGroup(page: Page, name: string, invite: string[] = [], description = "") {
  await page.goto("/c");
  await page.locator("main").click({ position: { x: 8, y: 300 } });
  await page.keyboard.press("Shift+N");
  const dialog = page.getByRole("dialog", { name: "New group" });
  await expect(dialog).toBeVisible();
  await dialog.getByLabel("Name").fill(name);
  if (description) await dialog.getByLabel("Description").fill(description);
  for (const id of invite) {
    await dialog.getByLabel("Invite", { exact: true }).fill(id);
    await dialog.getByLabel("Invite", { exact: true }).press("Enter");
  }
  await dialog.getByRole("button", { name: "Create group", exact: true }).click();
  await expect(page).toHaveURL(/\/c\/g:/);
  return decodeURIComponent(new URL(page.url()).pathname.replace(/^\/c\//, ""));
}

test("an owner renames a group, adds and removes members, makes an owner, and leaves", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "ada");
  const errors = watchErrors(page);
  const name = `Details check ${stamp()}`;
  const address = await createGroup(page, name);
  const panel = await openDetails(page);

  // Rename in place: the header, the panel and the service all have the new name; the address stays. A long name
  // wraps in the panel, never cut off with an ellipsis.
  const renamed = `${name} renamed, with a name long enough to need a second line in the panel`;
  await panel.getByRole("button", { name: `Group name: ${name}` }).click();
  const field = panel.getByRole("textbox", { name: "Group name" });
  await field.fill(renamed);
  await field.press("Enter");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(renamed);
  await expect.poll(async () => (await (await context.request.get(`/api/api/v1/groups/${encodeURIComponent(address)}`)).json()).data.group.name).toBe(renamed);
  await expect(panel.getByText(address, { exact: true })).toBeVisible();
  const title = panel.getByRole("button", { name: `Group name: ${renamed}` });
  await expect(title).toBeVisible();
  expect(await title.evaluate(node => [node, ...node.querySelectorAll("*")].filter(part => part.scrollWidth > part.clientWidth + 1).length)).toBe(0);

  // Add members, by keyboard: the field takes the keyboard (the button it replaced is gone). Your own Silicons join
  // at once, a Carbon is invited.
  await panel.getByRole("button", { name: "Add members" }).focus();
  await page.keyboard.press("Enter");
  const invite = panel.getByRole("combobox", { name: "Invite" });
  await expect(invite).toBeFocused();
  for (const id of ["si:dmc-scout", "si:dmc-quill", "c:dmc-bo"]) {
    await invite.fill(id);
    await invite.press("Enter");
  }
  await panel.getByRole("button", { name: "Invite", exact: true }).click();
  const members = panel.locator("section", { has: page.getByRole("heading", { name: /^Members/ }) });
  await expect(members.locator('li[data-account="si:dmc-scout"]')).toBeVisible();
  await expect(members.locator('li[data-account="si:dmc-quill"]')).toBeVisible();
  await expect(panel.locator("section", { has: page.getByRole("heading", { name: /^Invited/ }) }).locator('li[data-account="c:dmc-bo"]')).toBeVisible();

  // Make Scout an owner from its menu.
  await members.getByRole("button", { name: /^Actions for Scout/ }).click();
  await page.getByRole("menuitem", { name: "Make owner" }).click();
  await expect(members.locator('li[data-account="si:dmc-scout"]')).toContainText("Owner");

  // Remove Quill: the menu asks in place, and the row goes.
  await members.getByRole("button", { name: /^Actions for Quill/ }).click();
  await page.getByRole("menuitem", { name: "Remove from group" }).click();
  const quill = members.locator('li[data-account="si:dmc-quill"]');
  await expect(quill.getByText("Remove Quill?")).toBeVisible();
  await quill.getByRole("button", { name: "Remove", exact: true }).click();
  await expect(quill).toHaveCount(0);
  await expect.poll(async () => ((await (await context.request.get(`/api/api/v1/groups/${encodeURIComponent(address)}`)).json()).data.group.members as Array<{ id: string }>).map(member => member.id)).not.toContain("si:dmc-quill");

  // Withdraw Bo's invitation the same way (and keep Bo's invitations tidy on the shared stack).
  const invited = panel.locator("section", { has: page.getByRole("heading", { name: /^Invited/ }) });
  await invited.getByRole("button", { name: /^Actions for Bo/ }).click();
  await page.getByRole("menuitem", { name: "Withdraw invitation" }).click();
  const boRow = invited.locator('li[data-account="c:dmc-bo"]');
  await expect(boRow.getByText("Uninvite Bo?")).toBeVisible();
  await boRow.getByRole("button", { name: "Uninvite", exact: true }).click();
  await expect(boRow).toHaveCount(0);

  // Leave (held): back to the conversation list, and the group is no longer yours.
  const leave = panel.getByRole("button", { name: "Hold to leave the group" });
  await leave.scrollIntoViewIfNeeded();
  await leave.hover();
  await page.mouse.down();
  await page.waitForTimeout(1800);
  await page.mouse.up();
  await expect(page).toHaveURL(/\/c$/);
  // Ada can still read it as Scout's custodian (Scout owns it now), but she is no longer a member.
  await expect.poll(async () => ((await (await context.request.get(`/api/api/v1/groups/${encodeURIComponent(address)}`)).json()).data?.group?.members as Array<{ id: string }> | undefined ?? []).map(member => member.id)).not.toContain("c:dmc-ada");
  await expect(page.locator(`a[href="/c/${address}"]`)).toHaveCount(0);
  expect(errors).toEqual([]);
  await context.close();
});

test("bo invites ada to a group, and ada accepts from the Invitations dialog (G then I)", async ({ browser }) => {
  const bo = await signedIn(browser, "bo");
  // A long name and a description with a long link: the dialog wraps them instead of running past its edge.
  const name = `Invite check ${stamp()} for the offsite planning with a deliberately long name`;
  const description = "Rooms, trains and who brings what: https://example.com/a/very/long/path/that/keeps/going/without/any/spaces/at/all";
  const address = await createGroup(bo.page, name, ["c:dmc-ada"], description);
  await bo.context.close();

  const { context, page } = await signedIn(browser, "ada");
  const errors = watchErrors(page);
  await page.goto("/c");
  await page.locator("main").click({ position: { x: 8, y: 300 } });
  await page.keyboard.press("g");
  await page.keyboard.press("i");
  const dialog = page.getByRole("dialog", { name: "Invitations" });
  await expect(dialog).toBeVisible();
  const row = dialog.locator("li", { hasText: name });
  await expect(row).toContainText("Bo Okafor invited you");
  expect(await wideIn(page, "[role=dialog]")).toEqual([]);
  await row.getByRole("button", { name: "Accept", exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/c/${address.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(name);
  // The composer of the group has the keyboard, ready for the first word.
  await expect(page.locator("textarea[data-composer-field]")).toBeFocused();
  expect(errors).toEqual([]);
  // Ada leaves again, so the shared stack's lists do not fill with test groups.
  const left = await context.request.post(`/api/api/v1/groups/${encodeURIComponent(address)}/leave`, { headers: sameOrigin({ "x-dm-contract-version": "4" }) });
  expect(left.status()).toBe(204);
  await context.close();
});

test("di, who looks after no Silicon, starts a conversation with Scout; an id that names nobody is refused in the service's words first", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "di");
  const errors = watchErrors(page);
  try {
    await page.goto("/c");
    await page.locator("main").click({ position: { x: 8, y: 300 } });
    await page.keyboard.press("n");
    const dialog = page.getByRole("dialog", { name: "New conversation" });
    await expect(dialog).toBeVisible();
    // Any Carbon or Silicon: nothing in the dialog says a Silicon must allow you first.
    await expect(dialog).toContainText("With a Carbon or a Silicon.");
    await expect(dialog).not.toContainText(/allow/i);
    const to = dialog.getByRole("combobox", { name: "To" });
    await expect(to).toBeFocused();

    // An id that names nobody: the service's words, its chip marked, and the dialog stays.
    const nobody = `si:dmc-nobody-${stamp()}`;
    await to.fill(nobody);
    await to.press("Enter");
    await dialog.getByRole("button", { name: "Start conversation", exact: true }).click();
    const alert = dialog.getByRole("alert").filter({ hasText: "The conversation could not be opened" });
    await expect(alert).toContainText(`No Silicon Accounts account has the id ${nobody} now`);
    await expect(dialog).toBeVisible();
    await expect(page).toHaveURL(/\/c$/);

    // Scout, whom nobody asked to hear from di: the conversation opens (the one di has, from an earlier run) with its
    // field ready, and a message goes.
    await dialog.getByRole("button", { name: `Remove ${nobody}` }).click();
    await to.fill("si:dmc-scout");
    await to.press("Enter");
    await dialog.getByRole("button", { name: "Start conversation", exact: true }).click();
    await expect(dialog).toBeHidden();
    await expect(page).toHaveURL(/\/c\/(c:dmc-di::si:dmc-scout|si:dmc-scout::c:dmc-di)$/);
    const field = page.getByRole("textbox", { name: "Message Scout" });
    await expect(field).toBeFocused();
    const words = `Hello Scout from Di ${stamp()}`;
    await field.fill(words);
    await field.press("Enter");
    await expect(page.locator("article[data-message-row]", { hasText: words })).toHaveAttribute("data-message-code", /.+/);

    // Its details name who looks after Scout, and nothing about who may reach it.
    const panel = await openDetails(page);
    const about = panel.getByRole("region", { name: "About Scout" });
    await expect(about).toContainText("Ada Marlow (c:dmc-ada) looks after Scout and can read this conversation.");
    await expect(about).not.toContainText(/reach|allow/i);
    expect(errors).toEqual([]);
  } finally {
    await context.close();
  }
});

test("di invites Scout to a new group: the group opens, and Scout's invitation waits for its answer", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "di");
  const errors = watchErrors(page);
  let address = "";
  try {
    const name = `Stranger invite ${stamp()}`;
    address = await createGroup(page, name, ["si:dmc-scout", "c:dmc-bo"]);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(name);
    const panel = await openDetails(page);
    const invited = panel.locator("section", { has: page.getByRole("heading", { name: /^Invited/ }) });
    await expect(invited.locator('li[data-account="si:dmc-scout"]')).toBeVisible();
    await expect(invited.locator('li[data-account="c:dmc-bo"]')).toBeVisible();
    expect((await call(context, "GET", `/api/v1/groups/${encodeURIComponent(address)}`)).json?.data?.group).toMatchObject({ pending_invitations: expect.arrayContaining([expect.objectContaining({ id: "si:dmc-scout" })]) });
    expect(errors).toEqual([]);
  } finally {
    // Scout's invitation goes; Bo takes the group over (a group keeps an owner), as the other specs leave theirs.
    if (address) {
      const group = encodeURIComponent(address);
      await call(context, "DELETE", `/api/v1/groups/${group}/members`, { type: "remove_group_members", data: { member_ids: ["si:dmc-scout"] } });
      const bo = await signedIn(browser, "bo");
      await call(bo.context, "POST", `/api/v1/groups/${group}/accept`);
      await bo.context.close();
      await call(context, "POST", `/api/v1/groups/${group}/owners`, { type: "add_group_owners", data: { member_ids: ["c:dmc-bo"] } });
      await call(context, "POST", `/api/v1/groups/${group}/leave`);
    }
    await context.close();
  }
});

test("Settings and a Silicon's details have no reach: one fewer section, an old #reach link lands at the top", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "ada");
  const errors = watchErrors(page);
  const lists: string[] = [];
  page.on("request", request => { if (/\/api\/v1\/allow-list/.test(request.url())) lists.push(`${request.method()} ${request.url()}`); });
  try {
    // The address the details panel of 0.17.0 linked to: Settings at its top, Scout first in Voice.
    await page.goto("/settings?silicon=si:dmc-scout#reach");
    await expect(page.getByRole("heading", { level: 1, name: "Settings" })).toBeVisible();
    await settingsSettled(page);
    expect(await page.evaluate(() => document.getElementById("settings-scroll")!.scrollTop)).toBe(0);
    const nav = page.getByRole("navigation", { name: "Settings sections" });
    await expect(nav.getByRole("link")).toHaveText(["Voice", "Listening", "Sending", "Keyboard", "Notifications", "Account", "Report a problem"]);
    await expect(nav.getByRole("link", { name: "Voice" })).toHaveAttribute("aria-current", "page");
    await expect(page.locator("#settings-scroll").getByRole("heading", { level: 2 })).toHaveText(["Voice", "Listening", "Sending", "Keyboard", "Notifications", "Account", "Report a problem"]);
    await expect(page.locator("#reach")).toHaveCount(0);
    await expect(page.locator("#settings-scroll")).not.toContainText(/Silicons' reach|allow-list|may start a conversation/i);
    await expect(page.locator("#account")).toContainText(/You look after (Scout and Quill|Quill and Scout)\. You choose their voices, and you can read their conversations/);
    // A section still lands where it is asked for, and the spy agrees once the column has arrived.
    await nav.getByRole("link", { name: "Keyboard" }).click();
    await expect(page).toHaveURL(/#keyboard$/);
    await expect(nav.getByRole("link", { name: "Keyboard" })).toHaveAttribute("aria-current", "page");
    await page.waitForTimeout(1500);
    await expect(nav.getByRole("link", { name: "Keyboard" })).toHaveAttribute("aria-current", "page");

    // The details of Ada's own Silicon: who looks after it and its voice, then the address; no reach between them.
    await page.goto("/c/si:dmc-scout");
    const panel = await openDetails(page);
    const about = panel.getByRole("region", { name: "About Scout" });
    await expect(about).toContainText("You look after Scout.");
    await expect(about).not.toContainText(/reach|allow/i);
    await expect(panel.getByRole("link", { name: /reach/i })).toHaveCount(0);
    expect(lists, "the website no longer asks for allow-lists").toEqual([]);
    expect(errors).toEqual([]);
  } finally {
    await context.close();
  }
});

test("a rebound shortcut survives a reload and works, then every default comes back", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "ada");
  try {
    await page.goto("/settings#keyboard");
    // Every key a shortcut has shows, not only the one its recorder holds: ? and ⌘/ both open the shortcuts.
    await expect(page.locator('[data-shortcut="shortcuts.open"]').getByRole("img", { name: /^(Command|Control) Slash$/ })).toBeVisible();
    // By keyboard, as a Carbon would: the recorder takes focus, Enter starts recording, the next keys are the new ones.
    const recorder = page.getByRole("button", { name: /^Keys for New conversation/ });
    await recorder.focus();
    await page.keyboard.press("Enter");
    await expect(recorder).toHaveAttribute("aria-pressed", "true");
    await page.keyboard.press("Shift+K");
    await expect.poll(() => page.evaluate(() => window.localStorage.getItem("dm:shortcuts:v1"))).toContain("shift+k");
    await page.reload();
    await expect(page.getByRole("button", { name: /^Keys for New conversation/ })).toContainText("K");
    expect(await page.evaluate(() => JSON.parse(window.localStorage.getItem("dm:shortcuts:v1") ?? "{}"))).toEqual({ "conversation.new": ["shift+k"] });

    // The new keys open New conversation; the old N no longer does.
    await page.goto("/c");
    await page.locator("main").click({ position: { x: 8, y: 300 } });
    await page.keyboard.press("n");
    await expect(page.getByRole("dialog", { name: "New conversation" })).toHaveCount(0);
    await page.keyboard.press("Shift+K");
    await expect(page.getByRole("dialog", { name: "New conversation" })).toBeVisible();
    await page.keyboard.press("Escape");

    await page.goto("/settings#keyboard");
    await settingsSettled(page);
    await page.getByRole("button", { name: "Restore every default" }).click();
    await expect(page.getByText("Every shortcut has its default keys.")).toBeVisible();
  } finally {
    await page.evaluate(() => window.localStorage.removeItem("dm:shortcuts:v1")).catch(() => undefined);
    await context.close();
  }
});

test("dialogs give focus back to what opened them, and Esc clears a typed name before it closes the dialog", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "ada");
  await page.goto("/c");
  const newButton = page.getByRole("button", { name: "New conversation or group" });
  await newButton.click();
  await page.getByRole("menuitem", { name: "New conversation" }).click();
  const dialog = page.getByRole("dialog", { name: "New conversation" });
  const to = dialog.getByRole("combobox", { name: "To" });
  await expect(to).toBeFocused();
  await to.fill("bo");
  await expect(dialog.getByRole("option").first()).toContainText("Bo Okafor");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeVisible();
  await expect(to).toHaveValue("");
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(newButton).toBeFocused();

  // From the command palette: the palette closes as the dialog opens, and closing the dialog gives the keyboard back
  // to where it was before ⌘K (the message field here), not to the page.
  await page.goto("/c/c:dmc-ada::c:dmc-bo");
  const composer = page.locator("main textarea[data-composer-field]");
  await composer.focus();
  await page.keyboard.press(`${MOD}+K`);
  await expect(page.getByRole("dialog", { name: "Search conversations and actions" })).toBeVisible();
  await page.keyboard.type("Invitations");
  await page.keyboard.press("Enter");
  const invitations = page.getByRole("dialog", { name: "Invitations" });
  await expect(invitations).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(invitations).toHaveCount(0);
  await expect(composer).toBeFocused();
  await context.close();
});

test("in the drawer, Esc clears what was typed, then closes Add members, then the drawer", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "ada", { width: 900, height: 900 });
  const created = await call(context, "POST", "/api/v1/groups", { type: "create_group", data: { name: `Drawer check ${stamp()}`, member_ids: ["si:dmc-scout"] } });
  const address = created.json?.data?.id as string;
  try {
    await page.goto(`/c/${address}`);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await page.locator("main").click({ position: { x: 8, y: 300 } });
    await page.keyboard.press(`${MOD}+Period`);
    const panel = page.locator("[data-details-panel]");
    await expect(panel).toBeVisible();
    const add = panel.getByRole("button", { name: "Add members" });
    await add.click();
    const field = panel.getByRole("combobox", { name: "Invite" });
    await expect(field).toBeFocused();
    await page.keyboard.type("c:dmc");
    await page.keyboard.press("Escape");
    await expect(field).toHaveValue("");
    await expect(panel).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(field).toHaveCount(0);
    await expect(add).toBeFocused();
    await expect(panel).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(panel).toHaveCount(0);
  } finally {
    // Scout (Ada's own Silicon) keeps the group, so Ada can leave it and her list stays tidy.
    await call(context, "POST", `/api/v1/groups/${encodeURIComponent(address)}/owners`, { type: "add_group_owners", data: { member_ids: ["si:dmc-scout"] } });
    await call(context, "POST", `/api/v1/groups/${encodeURIComponent(address)}/leave`);
    await context.close();
  }
});

test("a custodian's view shows a Silicon's conversation read only, CLI commands included", async ({ browser }) => {
  // Bo and Scout talk; Ada looks after Scout and reads it as Scout's custodian.
  const bo = await signedIn(browser, "bo");
  const conversation = await call(bo.context, "POST", "/api/v1/conversations", { type: "create_conversation", data: { participant_ids: ["si:dmc-scout"] } });
  expect(conversation.status, JSON.stringify(conversation.json)).toBeLessThan(300);
  const address = conversation.json?.data?.id as string;
  await bo.context.close();

  const { context, page } = await signedIn(browser, "ada");
  await page.goto(`/c/${address}?for=si:dmc-scout`);
  const panel = await openDetails(page);
  await expect(panel.getByText(/You are reading Scout.s conversation as its custodian/)).toBeVisible();
  await expect(panel.getByRole("button", { name: /^Message / })).toHaveCount(0);
  const cli = panel.locator("pre");
  await expect(cli).toContainText("dm messages list");
  await expect(cli).toContainText("--for si:dmc-scout");
  await expect(cli).not.toContainText("messages send");
  await context.close();
});

test("a report's pull request link is checked before anything is sent", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "ada");
  const sent: string[] = [];
  page.on("request", request => { if (request.method() === "POST" && request.url().includes("/reports")) sent.push(request.url()); });
  await page.goto("/settings#report");
  await settingsSettled(page);
  await page.getByRole("button", { name: "Send report" }).click();
  await expect(page.getByLabel("What happened?")).toBeFocused();
  await expect(page.getByLabel("What happened?")).toHaveAccessibleDescription(/Say what happened first/);
  await page.getByLabel("What happened?").fill("A details check: nothing is wrong, this report is never sent.");
  await page.getByLabel("Pull request (optional)").fill("https://github.com/someone/elsewhere/pull/1");
  await page.getByRole("button", { name: "Send report" }).click();
  await expect(page.getByLabel("Pull request (optional)")).toBeFocused();
  await expect(page.getByLabel("Pull request (optional)")).toHaveAccessibleDescription(/pull requests in teamofsilicons\/silicon-dm, so the link looks like https:\/\/github\.com\/teamofsilicons\/silicon-dm\/pull\/123/);
  expect(sent).toEqual([]);
  await context.close();
});

test("the details panel never scrolls sideways on a phone (390 px)", async ({ browser }) => {
  const { context, page } = await signedIn(browser, "ada", { width: 390, height: 844 });
  // A group with a long name, a long unbroken word and a link in its description, besides two direct conversations.
  const created = await call(context, "POST", "/api/v1/groups", { type: "create_group", data: {
    name: `Phone check ${stamp()} with an unbreakablewordthatgoesonandonandonforever`,
    description: "Plans 🚆 and a link: https://example.com/a/very/long/path/that/keeps/going/without/any/spaces/at/all",
    member_ids: ["si:dmc-scout"],
  } });
  const group = created.json?.data?.id as string;
  for (const address of ["c:dmc-ada::si:dmc-scout", "c:dmc-ada::c:dmc-bo", group]) {
    await page.goto(`/c/${address}`);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await page.keyboard.press(`${MOD}+Period`);
    const panel = page.locator("[data-details-panel]");
    await expect(panel).toBeVisible();
    await expect(panel.getByRole("heading", { name: "Address" })).toBeVisible();
    const overflow = await page.evaluate(() => {
      const panel = document.querySelector<HTMLElement>("[data-details-panel]")!;
      const wide: string[] = [];
      for (const node of [panel, ...panel.querySelectorAll<HTMLElement>("*")]) {
        const box = node.getBoundingClientRect();
        if (box.width && box.right > window.innerWidth + 0.5) wide.push(`${node.tagName.toLowerCase()}.${node.className}`);
        const style = getComputedStyle(node);
        if (/(auto|scroll)/.test(style.overflowX) && node.scrollWidth > node.clientWidth + 1) wide.push(`${node.tagName.toLowerCase()} scrolls sideways`);
      }
      return { wide: wide.slice(0, 5), page: document.documentElement.scrollWidth > window.innerWidth };
    });
    expect(overflow, address).toEqual({ wide: [], page: false });
    await page.keyboard.press("Escape");
  }
  await call(context, "POST", `/api/v1/groups/${encodeURIComponent(group)}/owners`, { type: "add_group_owners", data: { member_ids: ["si:dmc-scout"] } });
  await call(context, "POST", `/api/v1/groups/${encodeURIComponent(group)}/leave`);
  await context.close();
});

/** axe-core's WCAG 2.2 A and AA rules on one part of the page (the details area's own surfaces). */
async function audit(page: Page, selector: string, label: string) {
  await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== "running"), undefined, { timeout: 5_000 }).catch(() => undefined);
  await page.waitForTimeout(300);
  await page.addScriptTag({ path: join(process.cwd(), "node_modules", "axe-core", "axe.min.js") });
  const violations = await page.evaluate(async ({ selector, tags }) => {
    const axe = (window as unknown as { axe: { run: (context: object, options: object) => Promise<{ violations: Array<{ id: string; help: string; nodes: Array<{ target: string[] }> }> }> } }).axe;
    const result = await axe.run({ include: [[selector]] }, { runOnly: { type: "tag", values: tags }, resultTypes: ["violations"] });
    return result.violations.map(violation => `${violation.id}: ${violation.help} (${violation.nodes.slice(0, 3).map(node => node.target.join(" ")).join(", ")})`);
  }, { selector, tags: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"] });
  expect(violations, label).toEqual([]);
}

for (const phone of [false, true]) {
  test(`the details area passes WCAG 2.2 AA (${phone ? "phone" : "desktop"})`, async ({ browser }) => {
    const viewport = phone ? { width: 390, height: 844 } : { width: 1440, height: 900 };
    const { account, ...cookie } = JSON.parse(execFileSync("node", [HELPER, "cookie", "ada"], { encoding: "utf8" })) as { account: unknown; name: string; value: string };
    void account;
    // axe's script is injected, so the page's CSP is set aside for this test only.
    const context = await browser.newContext({ baseURL: BASE, viewport, bypassCSP: true, reducedMotion: "reduce", locale: "en-US", timezoneId: "Asia/Kolkata" });
    await context.addCookies([cookie as never]);
    const page = await context.newPage();
    await page.goto("/settings");
    await audit(page, "main", "settings");
    for (const path of ["/c?new=1", "/c?new=group"]) {
      await page.goto(path);
      await expect(page.getByRole("dialog")).toBeVisible();
      await audit(page, "[role=dialog]", path);
    }
    await page.goto("/c");
    await page.locator("main").click({ position: { x: 8, y: 300 } });
    await page.keyboard.press("g");
    await page.keyboard.press("i");
    await expect(page.getByRole("dialog", { name: "Invitations" })).toBeVisible();
    await audit(page, "[role=dialog]", "invitations");
    for (const address of ["c:dmc-ada::si:dmc-scout", "c:dmc-ada::c:dmc-bo"]) {
      await page.goto(`/c/${address}`);
      await openDetails(page);
      await audit(page, "[data-details-panel]", `details of ${address}`);
    }
    await context.close();
  });
}
