/**
 * Conversations that start, change or end somewhere else reach an open list within seconds, without a reload.
 *   - With conversation events (DM 0.17 announces `features.conversation_events` in GET /api/v1/contracts), the sync
 *     feed says so itself: an invitation, a group joined, renamed or left on another device (lib/chat/sync.ts).
 *   - Against an older service (held here by a page whose contracts say nothing of them) the website listens three
 *     slower ways: a sync event naming a conversation the list does not have refreshes the list; an invitation that
 *     goes without this tab answering it was accepted elsewhere (invitations are asked for every 15 s); and another
 *     tab of this browser says when it starts, joins or leaves a conversation (lib/chat/news.ts).
 * Every run makes its own groups and asserts only on them (the stack is shared).
 */
import { expect, test, type BrowserContext, type Page } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

/** The DM service as a signed-in browser reaches it (the website's proxy), the way another device would. */
async function api<T = { id: string }>(context: BrowserContext, method: string, path: string, data?: unknown): Promise<T> {
  const response = await context.request.fetch(`/api${path}`, {
    method,
    headers: sameOrigin({ "Idempotency-Key": crypto.randomUUID(), ...(data === undefined ? {} : { "Content-Type": "application/json" }) }),
    data: data === undefined ? undefined : JSON.stringify(data),
  });
  const body = (await response.json().catch(() => null)) as { data?: T } | null;
  expect(response.ok(), `${method} ${path} answered ${response.status()}: ${JSON.stringify(body)}`).toBe(true);
  return body?.data as T;
}

/** A row of the list on screen (the inbox page keeps a phone copy of the list in the page, hidden on a desktop). */
const row = (page: Page, name: string) => page.locator("a[data-row]:visible", { hasText: name });

/** Holds this page to a service without conversation events: its contracts say nothing of them. */
async function withoutConversationEvents(page: Page): Promise<void> {
  await page.route(url => url.pathname === "/api/api/v1/contracts", async route => {
    const response = await route.fetch();
    const body = (await response.json().catch(() => null)) as { data?: { features?: Record<string, unknown> } } | null;
    if (body?.data?.features) delete body.data.features.conversation_events;
    await route.fulfill({ response, json: body });
  });
}

/** Whether the stack's DM service announces conversation events in its sync feed. */
async function serviceHasConversationEvents(context: BrowserContext): Promise<boolean> {
  const response = await context.request.get("/api/api/v1/contracts", { headers: sameOrigin({ "x-dm-contract-version": "4" }) });
  const body = (await response.json().catch(() => null)) as { data?: { features?: { conversation_events?: { sync_query?: string } } } } | null;
  return body?.data?.features?.conversation_events?.sync_query === "include=conversations";
}

test("without conversation events, a group joined on another device shows in the open list within 5 s of its first message", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const ada = await accountsSession(browser, "carbon");
  const bo = await accountsSession(browser, "friend");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  const friend = await bo.newPage();
  await signIn(page, runAccounts().carbon.email);
  await signIn(friend, runAccounts().friend.email);
  // Only a message in the sync feed may bring the group in here: invitations are held still, so no refresh comes from
  // them, and the service's conversation events are not asked for.
  await page.route(url => url.pathname === "/api/api/v1/invitations", route => route.fulfill({ json: { type: "invitations", data: { items: [] } } }));
  await withoutConversationEvents(page);
  await page.goto("/c");
  await expect(page.getByRole("navigation", { name: "Conversations" })).toBeVisible();

  // Bo makes a group with Ada in it (a Carbon is invited and joins when she accepts), and she accepts on her phone.
  const name = `Sync check ${stamp}`;
  const group = await api(bo, "POST", "/api/v1/groups", { type: "create_group", data: { name, member_ids: [runAccounts().carbon.id] } });
  await api(ada, "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/accept`);
  await expect(row(page, name)).toHaveCount(0);

  // Bo says hello: the sync feed names a conversation Ada's list does not have, and the list asks again.
  const hello = `Hello from Bo ${stamp}`;
  const sent = Date.now();
  await api(bo, "POST", `/api/v1/conversations/${encodeURIComponent(group.id)}/messages`, { type: "message.create", data: { message: hello } });
  await expect(row(page, name)).toContainText(hello, { timeout: 5_000 });
  const took = Date.now() - sent;
  test.info().annotations.push({ type: "latency", description: `the group reached the open list ${took} ms after Bo's message` });
  expect(took, "the group reached the open list").toBeLessThan(5_000);
  expect(new URL(page.url()).pathname, "no reload, no navigation").toBe("/c");

  await api(ada, "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/leave`);
  expect(errors).toEqual([]);
  await ada.close();
  await bo.close();
});

test("without conversation events, an invitation shows in the open sidebar, and accepting it on another device brings the group in", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const ada = await accountsSession(browser, "carbon");
  const bo = await accountsSession(browser, "friend");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  const friend = await bo.newPage();
  await signIn(page, runAccounts().carbon.email);
  await signIn(friend, runAccounts().friend.email);
  await withoutConversationEvents(page);
  await page.goto("/c");
  await expect(page.getByRole("navigation", { name: "Conversations" })).toBeVisible();

  const name = `Invite check ${stamp}`;
  const group = await api(bo, "POST", "/api/v1/groups", { type: "create_group", data: { name, member_ids: [runAccounts().carbon.id] } });
  // Invitations are asked for every 15 s while the tab shows: the sidebar offers it without a reload.
  // (Other runs may have left Ada invitations already: the dialog, once open, keeps asking and shows this one.)
  const invitations = page.getByRole("button", { name: /^Invitations \d+ waiting$/ });
  await expect(invitations).toBeVisible({ timeout: 25_000 });
  await invitations.click();
  const dialog = page.getByRole("dialog", { name: "Invitations" });
  await expect(dialog.locator("li", { hasText: name })).toBeVisible({ timeout: 25_000 });
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);

  // Accepted on another device: the invitation goes from this tab's list at its next check, and the group comes in.
  await api(ada, "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/accept`);
  await expect(row(page, name)).toBeVisible({ timeout: 25_000 });
  expect(new URL(page.url()).pathname).toBe("/c");

  await api(ada, "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/leave`);
  expect(errors).toEqual([]);
  await ada.close();
  await bo.close();
});

test("a group joined in another tab of this browser shows in this tab at once", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const ada = await accountsSession(browser, "carbon");
  const bo = await accountsSession(browser, "friend");
  const first = await ada.newPage();
  const errors = watchErrors(first);
  // The other tab's news is what this test is about, so this tab does not hear the service's conversation events.
  await withoutConversationEvents(first);
  await signIn(first, runAccounts().carbon.email);
  await signIn(await bo.newPage(), runAccounts().friend.email);
  const name = `Tab check ${stamp}`;
  const group = await api(bo, "POST", "/api/v1/groups", { type: "create_group", data: { name, member_ids: [runAccounts().carbon.id] } });
  const second = await ada.newPage();
  await second.goto("/c");
  await first.goto("/c");
  await expect(first.getByRole("navigation", { name: "Conversations" })).toBeVisible();

  // The other tab accepts the invitation (G then I opens them).
  await second.getByRole("main").click({ position: { x: 20, y: 120 } });
  await second.keyboard.press("g");
  await second.keyboard.press("i");
  const invitation = second.getByRole("dialog", { name: "Invitations" }).locator("li", { hasText: name });
  await invitation.getByRole("button", { name: "Accept", exact: true }).click();
  await expect(second).toHaveURL(url => decodeURIComponent(url.pathname) === `/c/${group.id}`);
  const joined = Date.now();

  // This tab hears it from the other one: the group is in its list within a couple of seconds, with no reload.
  await expect(row(first, name)).toBeVisible({ timeout: 3_000 });
  const took = Date.now() - joined;
  test.info().annotations.push({ type: "latency", description: `the group reached the other tab's list ${took} ms after it opened` });
  expect(took).toBeLessThan(3_000);
  expect(new URL(first.url()).pathname).toBe("/c");

  await api(ada, "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/leave`);
  expect(errors).toEqual([]);
  await ada.close();
  await bo.close();
});

test("with conversation events, an invitation, a join, a rename and a removal on another device reach the open list within seconds", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const ada = await accountsSession(browser, "carbon");
  const bo = await accountsSession(browser, "friend");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  const friend = await bo.newPage();
  await signIn(page, runAccounts().carbon.email);
  test.skip(!(await serviceHasConversationEvents(ada)), "the DM service on the stack does not announce conversation events");
  await signIn(friend, runAccounts().friend.email);
  const reading = page.waitForRequest(request => request.url().includes("/api/api/v1/sync?") && request.url().includes("include=conversations"));
  await page.goto("/c");
  await expect(page.getByRole("navigation", { name: "Conversations" })).toBeVisible();
  await reading;
  const timings: string[] = [];
  const within = async (what: string, since: number, check: () => Promise<void>) => {
    await check();
    const took = Date.now() - since;
    timings.push(`${what} ${took} ms`);
    expect(took, what).toBeLessThan(5_000);
  };

  // Bo invites Ada: the open invitations show it (they are no longer asked for every 15 s; the event brings it).
  await page.getByRole("main").click({ position: { x: 20, y: 120 } });
  await page.keyboard.press("g");
  await page.keyboard.press("i");
  const dialog = page.getByRole("dialog", { name: "Invitations" });
  await expect(dialog).toBeVisible();
  const name = `Event check ${stamp}`;
  let at = Date.now();
  const group = await api(bo, "POST", "/api/v1/groups", { type: "create_group", data: { name, member_ids: [runAccounts().carbon.id] } });
  await within("the invitation", at, () => expect(dialog.locator("li", { hasText: name })).toBeVisible({ timeout: 5_000 }));
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);

  // Accepted on another device, with nothing said in the group yet: it joins the list.
  at = Date.now();
  await api(ada, "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/accept`);
  await within("the join", at, () => expect(row(page, name)).toBeVisible({ timeout: 5_000 }));

  // Bo renames it: the row says the new name, in its place.
  const renamed = `Event renamed ${stamp}`;
  const current = await api<{ group: { version: number } }>(bo, "GET", `/api/v1/groups/${encodeURIComponent(group.id)}`);
  at = Date.now();
  const rename = await bo.request.fetch(`/api/api/v1/groups/${encodeURIComponent(group.id)}`, {
    method: "PATCH",
    headers: sameOrigin({ "Content-Type": "application/json", "Idempotency-Key": crypto.randomUUID(), "If-Match": String(current.group.version), "x-dm-contract-version": "4" }),
    data: JSON.stringify({ type: "update_group", data: { name: renamed } }),
  });
  expect(rename.ok(), `the rename answered ${rename.status()}`).toBe(true);
  await within("the rename", at, () => expect(row(page, renamed)).toBeVisible({ timeout: 5_000 }));

  // Bo takes Ada out while she has the group open: the row goes, and so does the conversation, with the reason.
  await row(page, renamed).click();
  await expect(page).toHaveURL(url => decodeURIComponent(url.pathname) === `/c/${group.id}`);
  at = Date.now();
  const removal = await bo.request.fetch(`/api/api/v1/groups/${encodeURIComponent(group.id)}/members`, {
    method: "DELETE",
    headers: sameOrigin({ "Content-Type": "application/json", "Idempotency-Key": crypto.randomUUID(), "x-dm-contract-version": "4" }),
    data: JSON.stringify({ type: "remove_group_members", data: { member_ids: [runAccounts().carbon.id] } }),
  });
  expect(removal.ok(), `the removal answered ${removal.status()}`).toBe(true);
  await within("the removal", at, () => expect(row(page, renamed)).toHaveCount(0, { timeout: 5_000 }));
  await expect(page).toHaveURL(url => url.pathname === "/c");
  await expect(page.getByText(`You are no longer in ${renamed}`)).toBeVisible();
  test.info().annotations.push({ type: "latency", description: timings.join(", ") });

  expect(errors).toEqual([]);
  await ada.close();
  await bo.close();
});
