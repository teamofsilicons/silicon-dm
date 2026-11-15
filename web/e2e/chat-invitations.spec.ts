import { expect, test, type BrowserContext } from "@playwright/test";
import { accountsSession, captureViews, runAccounts, sameOrigin, sessionOf, signIn, signInOnHostedPages, uniqueEmail } from "./support";

async function invite(context: BrowserContext, to: string) {
  const response = await context.request.post("/api/api/v1/chat-invitations", {
    headers: sameOrigin({ "idempotency-key": crypto.randomUUID(), "x-dm-contract-version": "4" }),
    data: { type: "chat_invitation.create", data: { to } },
  });
  expect(response.status()).toBe(201);
  return (await response.json()).data as { id: string; conversation_id: string | null; mail_status: string };
}

test("invite by ID, show both identities, and open the same chat after acceptance", async ({ browser }) => {
  const sender = await accountsSession(browser, "carbon");
  const recipient = await accountsSession(browser, "friend");
  try {
    const page = await sender.newPage(); const other = await recipient.newPage();
    await signIn(page, runAccounts().carbon.email);
    await signIn(other, runAccounts().friend.email);
    await page.getByRole("button", { name: "New conversation or group" }).click();
    await page.getByRole("menuitem", { name: "Invite to chat" }).click();
    const dialog = page.getByRole("dialog", { name: "Invite to chat" });
    await expect(dialog.getByLabel("Invite", { exact: true })).toBeFocused();
    await dialog.getByLabel("Invite", { exact: true }).fill(runAccounts().friend.id);
    await captureViews(page, "chat-invite-form");
    const sent = page.waitForResponse(response => response.url().endsWith("/api/api/v1/chat-invitations") && response.request().method() === "POST");
    await dialog.getByRole("button", { name: "Send invitation" }).click();
    const response = await sent;
    expect(response.status()).toBe(201);
    const invitation = (await response.json()).data as { id: string };
    await expect(dialog.getByRole("status")).toContainText("Invitation created");
    await other.reload();
    await other.getByRole("button", { name: /^Invitations/ }).click();
    const row = other.locator(`[data-chat-invitation="${invitation.id}"]`);
    await expect(row).toContainText(runAccounts().carbon.id);
    await captureViews(other, "chat-invite-incoming");
    await row.getByRole("button", { name: "Open chat" }).click();
    await expect(other).toHaveURL(/\/c\/[^/?]+/);
    const conversation = other.url();
    await other.goto(`/invite/${invitation.id}`);
    await expect(other).toHaveURL(conversation);
    await expect(other.locator("textarea[data-composer-field]")).toBeVisible();
  } finally { await sender.close(); await recipient.close(); }
});

test("an emailed invitation survives new-account signup and automatically opens the chat", async ({ browser }) => {
  const sender = await accountsSession(browser, "carbon");
  const newcomer = await browser.newContext({ baseURL: process.env.E2E_BASE_URL });
  try {
    const page = await sender.newPage(); await signIn(page, runAccounts().carbon.email);
    const email = uniqueEmail("chat-invite");
    const invitation = await invite(sender, email);
    expect(invitation.mail_status).toBe("simulated");
    // Possession of the link cannot grant the sender access as the recipient.
    await page.goto(`/invite/${invitation.id}`);
    await expect(page.getByRole("main").getByRole("alert")).toContainText("different account or email");
    const accept = await newcomer.newPage();
    accept.setDefaultTimeout(15_000);
    await accept.goto(`/invite/${invitation.id}`);
    await signInOnHostedPages(accept, email, { shareEmail: true }).catch(async error => {
      await accept.screenshot({ path: test.info().outputPath("signup-failure.png"), fullPage: true });
      throw error;
    });
    await expect(accept).toHaveURL(/\/c\/[^/?]+/, { timeout: 30_000 });
    await expect(accept.locator("textarea[data-composer-field]")).toBeVisible();
    const session = await sessionOf(newcomer);
    expect(session.signed_in).toBe(true);
    const result = await newcomer.request.get(`/api/api/v1/chat-invitations/${invitation.id}`);
    expect(result.status()).toBe(200);
    const accepted = (await result.json()).data;
    expect(accepted.status).toBe("accepted");
    expect(accepted.conversation_id).toContain(session.account!.id);
    expect(accepted.conversation_id).toContain(runAccounts().carbon.id);
    await accept.reload();
    await expect(accept.locator("textarea[data-composer-field]")).toBeVisible();
  } finally { await sender.close(); await newcomer.close(); }
});
