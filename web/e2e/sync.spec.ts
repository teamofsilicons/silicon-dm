/**
 * Conversations that start somewhere else reach an open list within seconds, without a reload. The DM service's sync
 * feed carries messages, not new conversations, so the website listens three ways:
 *   - a sync event naming a conversation the list does not have refreshes the list (lib/chat/sync.ts);
 *   - an invitation that goes without this tab answering it was accepted elsewhere, and refreshes the list too
 *     (components/chat/shell/chat-runtime.tsx; invitations are asked for every 15 s);
 *   - another tab of this browser says when it starts, joins or leaves a conversation (lib/chat/news.ts).
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

test("a group joined on another device shows in the open list within 5 s of its first message", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const ada = await accountsSession(browser, "carbon");
  const bo = await accountsSession(browser, "friend");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  const friend = await bo.newPage();
  await signIn(page, runAccounts().carbon.email);
  await signIn(friend, runAccounts().friend.email);
  // Only the sync feed may bring the group in here: invitations are held still, so no refresh comes from them.
  await page.route(url => url.pathname === "/api/api/v1/invitations", route => route.fulfill({ json: { type: "invitations", data: { items: [] } } }));
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

test("an invitation shows in the open sidebar, and accepting it on another device brings the group in", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const ada = await accountsSession(browser, "carbon");
  const bo = await accountsSession(browser, "friend");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  const friend = await bo.newPage();
  await signIn(page, runAccounts().carbon.email);
  await signIn(friend, runAccounts().friend.email);
  await page.goto("/c");
  await expect(page.getByRole("navigation", { name: "Conversations" })).toBeVisible();

  const name = `Invite check ${stamp}`;
  const group = await api(bo, "POST", "/api/v1/groups", { type: "create_group", data: { name, member_ids: [runAccounts().carbon.id] } });
  // Invitations are asked for every 15 s while the tab shows: the sidebar offers it without a reload.
  // (Other runs may have left Ada invitations already: the dialog, once open, keeps asking and shows this one.)
  const invitations = page.getByRole("button", { name: /^\d+ invitations?$/ });
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
