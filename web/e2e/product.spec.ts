/**
 * The chat end to end, as two Carbons use it: a new conversation from the keyboard (N), a message that shows at once
 * and reaches the other browser live, a reply, an edit with ↑, a draft that survives a reload, deleting after asking
 * in place, a group from ⇧N whose invitation the other accepts, the shortcuts and the palette. Then the API proxy's
 * same-origin rule, and a report from the settings that reaches the DM service.
 */
import { expect, test } from "@playwright/test";
import { accountsSession, captureViews, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

test("two Carbons talk: a conversation, live delivery, a reply, an edit, a draft, a deletion and a group", async ({ browser }) => {
  const a = await accountsSession(browser, "carbon");
  const b = await accountsSession(browser, "friend");
  const page = await a.newPage();
  const friend = await b.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().carbon.email);
  await signIn(friend, runAccounts().friend.email);
  const stamp = Date.now().toString(36);

  // N opens New conversation with the field ready; the conversation opens at its own address.
  await page.keyboard.press("n");
  const start = page.getByRole("dialog", { name: "New conversation" });
  await expect(start).toBeVisible();
  await page.keyboard.type(runAccounts().friend.id);
  await page.keyboard.press("Enter");
  await start.getByRole("button", { name: "Start conversation", exact: true }).click();
  await expect(page).toHaveURL(/\/c\/c:.+::c:.+/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();

  // A message shows at once (the outbox), then as the service has it.
  const field = page.getByRole("textbox", { name: /^Message / });
  const first = `A thought worth keeping ${stamp}`;
  await field.fill(first);
  await field.press("Enter");
  await expect(field).toHaveValue("");
  const sent = page.locator("article[data-message-code]", { hasText: first });
  await expect(sent).toBeVisible();
  await captureViews(page, "conversation");

  // The other Carbon has it live, replies from the message's toolbar, and the first sees Read.
  await friend.goto(page.url());
  const received = friend.locator("article[data-message-code]", { hasText: first });
  await expect(received).toBeVisible();
  await received.hover();
  await received.getByRole("button", { name: "Reply" }).click();
  await expect(friend.getByText(/^Replying to /)).toBeVisible();
  const reply = friend.getByRole("textbox", { name: /^Message / });
  await reply.fill(`Got it, thanks! ${stamp}`);
  await reply.press("Enter");
  await expect(page.locator("article[data-message-code]", { hasText: `Got it, thanks! ${stamp}` }).getByRole("button", { name: /^Reply to / })).toBeVisible();
  await expect(page.locator(`article[data-status="read"]`, { hasText: first })).toBeVisible();

  // ↑ in the empty field edits the last message; it says Edited.
  await field.click();
  await field.press("ArrowUp");
  const edit = page.getByRole("textbox", { name: "Edit your message" });
  await edit.fill(`A thought, made clearer ${stamp}`);
  await edit.press("Enter");
  const edited = page.locator("article[data-message-code]", { hasText: `A thought, made clearer ${stamp}` });
  await expect(edited.getByRole("button", { name: "Edited" })).toBeVisible();

  // ⌘↑ selects the last message; I shows its details with the CLI commands.
  await field.click();
  await field.press(process.platform === "darwin" ? "Meta+ArrowUp" : "Control+ArrowUp");
  await page.keyboard.press("i");
  const details = page.getByRole("dialog", { name: /^Message / });
  await expect(details).toContainText("dm messages show");
  await page.keyboard.press("Escape");

  // Deleting asks in place, then leaves a tombstone.
  const doomed = `Sent to the wrong person ${stamp}`;
  await field.fill(doomed);
  await field.press("Enter");
  await expect(page.locator("article[data-message-code]", { hasText: doomed })).toBeVisible();
  await field.press(process.platform === "darwin" ? "Meta+ArrowUp" : "Control+ArrowUp");
  await page.keyboard.press("Delete");
  await expect(page.getByText("Delete for everyone?")).toBeVisible();
  await page.getByRole("button", { name: "Delete", exact: true }).last().click();
  await expect(page.getByText(doomed)).toHaveCount(0);

  // A draft is kept in the browser and on the service: it is back after a reload.
  await field.fill("A draft for later");
  await page.waitForTimeout(2200);
  await page.reload();
  await expect(page.getByRole("textbox", { name: /^Message / })).toHaveValue("A draft for later");
  await page.getByRole("textbox", { name: /^Message / }).fill("");

  // ⇧N: a group; the other Carbon accepts its invitation from the list.
  await page.getByRole("main").click({ position: { x: 20, y: 120 } });
  await page.keyboard.press("Shift+N");
  const group = page.getByRole("dialog", { name: "New group" });
  await group.getByLabel("Name").fill(`Working notes ${stamp}`);
  await group.getByLabel("Invite", { exact: true }).fill(runAccounts().friend.id);
  await group.getByLabel("Invite", { exact: true }).press("Enter");
  await group.getByRole("button", { name: "Create group", exact: true }).click();
  await expect(page).toHaveURL(/\/c\/g:working-notes/);
  await captureViews(page, "group");

  await friend.goto("/c");
  await friend.getByRole("button", { name: /^Invitations \d+ waiting$/ }).click();
  const invitations = friend.getByRole("dialog", { name: "Invitations" });
  await invitations.locator("li", { hasText: `Working notes ${stamp}` }).getByRole("button", { name: "Accept", exact: true }).click();
  await expect(friend).toHaveURL(/\/c\/g:working-notes/);

  // The shortcuts and the palette.
  await page.getByRole("main").click({ position: { x: 20, y: 120 } });
  await page.keyboard.press("Shift+?");
  await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.keyboard.press(process.platform === "darwin" ? "Meta+K" : "Control+K");
  await expect(page.getByRole("dialog", { name: "Search conversations and actions" })).toBeVisible();
  await page.keyboard.type("Settings");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/settings/);

  // The API proxy refuses a write from another site, and answers this one.
  const rejected = await a.request.post("/api/api/v1/groups", { headers: { Origin: "https://elsewhere.example" }, data: { type: "create_group", data: { name: "Forbidden" } } });
  expect(rejected.status()).toBe(403);
  const good = await a.request.get("/api/api/v1/me");
  expect(good.status()).toBe(200);
  expect((await good.json()).data.account.id).toBe(runAccounts().carbon.id);

  await a.request.post("/auth/sign-out", { headers: sameOrigin() });
  expect(errors).toEqual([]);
  await a.close();
  await b.close();
});

test("a report from the settings reaches the DM service and says so", async ({ browser }) => {
  const a = await accountsSession(browser, "carbon");
  const page = await a.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().carbon.email);
  await page.goto("/settings#report");
  await page.getByLabel("What happened?").fill(`Browser verification report ${Date.now().toString(36)}`);
  const answer = page.waitForResponse(response => response.request().method() === "POST" && new URL(response.url()).pathname.endsWith("/reports"));
  await page.getByRole("button", { name: "Send report" }).click();
  // Only the real success counts. The service allows a few reports an hour per account (DM_REPORTS_PER_HOUR); every run
  // reports as the same Carbon, so the local chat stack raises it in .dev/dm-api.env.
  const response = await answer;
  expect(response.status(), `POST /reports answered ${response.status()}: ${await response.text()}${response.status() === 429 ? " (the stack's report limit is used up: raise DM_REPORTS_PER_HOUR in .dev/dm-api.env, then scripts/chat-stack/up.sh)" : ""}`).toBeLessThan(300);
  await expect(page.getByRole("status").filter({ hasText: /^Thank you\. The report was sent/ })).toBeVisible();
  expect(errors).toEqual([]);
  await a.close();
});

test("old addresses lead to the chat", async ({ browser }) => {
  const a = await accountsSession(browser, "carbon");
  const page = await a.newPage();
  await signIn(page, runAccounts().carbon.email);
  await page.goto("/conversations");
  await expect(page).toHaveURL(/\/c$/);
  await page.goto("/groups");
  await expect(page).toHaveURL(/\/c$/);
  await page.goto("/c?new=group");
  await expect(page.getByRole("dialog", { name: "New group" })).toBeVisible();
  await a.close();
});
