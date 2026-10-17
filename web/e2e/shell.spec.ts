/**
 * The shell's promise about toasts: they never cover the message field or its buttons. With the sidebar on screen
 * they rise in its column above the account bar; on a phone, just above the composer (components/chat/shell/
 * toast-place.tsx). Checked with the details panel open, at 900 px, at 390 px, and offline (when the offline banner
 * shows). The run makes its own group.
 */
import { expect, test, type Page } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

const MOD = process.platform === "darwin" ? "Meta" : "Control";

type Box = { x: number; y: number; width: number; height: number };
const overlaps = (a: Box, b: Box) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;

/** Raises a toast the way a Carbon does (⌘K → Copy this conversation's address) and returns where it is. */
async function raiseToast(page: Page): Promise<Box> {
  await page.getByRole("main").click({ position: { x: 8, y: 300 } });
  await page.keyboard.press(`${MOD}+K`);
  const palette = page.getByRole("dialog", { name: "Search conversations and actions" });
  await expect(palette).toBeVisible();
  await page.keyboard.type("Copy this conversation's address");
  await page.keyboard.press("Enter");
  await expect(palette).toHaveCount(0);
  const toast = page.getByRole("region", { name: /^Notifications/ }).getByRole("listitem").filter({ hasText: "Address copied" }).first();
  await expect(toast).toBeVisible();
  // Let it settle where it rises to (it springs in from below).
  await page.waitForTimeout(700);
  return (await toast.boundingBox())!;
}

test("toasts never cover the composer: beside it on wide screens, above it on a phone, offline too", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const ada = await accountsSession(browser, "carbon", { reducedMotion: "reduce" });
  const bo = await accountsSession(browser, "friend");
  await ada.grantPermissions(["clipboard-read", "clipboard-write"]);
  const page = await ada.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().carbon.email);
  await signIn(await bo.newPage(), runAccounts().friend.email);
  // Bo's group with Ada in it, so Ada can leave it afterwards (Bo stays its owner) and her list stays clean.
  const write = { headers: sameOrigin({ "Idempotency-Key": crypto.randomUUID(), "Content-Type": "application/json" }) };
  const created = await bo.request.post("/api/api/v1/groups", { ...write, data: JSON.stringify({ type: "create_group", data: { name: `Toast check ${stamp}`, member_ids: [runAccounts().carbon.id] } }) });
  expect(created.ok(), await created.text()).toBe(true);
  const group = ((await created.json()) as { data: { id: string } }).data.id;
  const accepted = await ada.request.post(`/api/api/v1/groups/${encodeURIComponent(group)}/accept`, { headers: sameOrigin() });
  expect(accepted.ok(), await accepted.text()).toBe(true);
  await page.goto(`/c/${group}`);
  const composer = page.locator("[data-composer]");
  await expect(composer).toBeVisible();

  // 1440 with the details panel open: the toast sits in the sidebar's column, clear of the composer and the panel.
  await page.getByRole("main").click({ position: { x: 8, y: 300 } });
  await page.keyboard.press(`${MOD}+Period`);
  const panel = page.locator("[data-details-panel]");
  await expect(panel).toBeVisible();
  let toast = await raiseToast(page);
  expect(overlaps(toast, (await composer.boundingBox())!), "the toast covers the composer at 1440").toBe(false);
  expect(overlaps(toast, (await panel.boundingBox())!), "the toast covers the details panel").toBe(false);
  const sidebar = (await page.locator("[data-pane='sidebar']").boundingBox())!;
  expect(toast.x + toast.width, "the toast stays in the sidebar's column").toBeLessThanOrEqual(sidebar.x + sidebar.width);
  await page.keyboard.press(`${MOD}+Period`);

  // A medium window, and a phone, offline: above the composer (and still clear of it), on screen.
  for (const viewport of [{ width: 900, height: 800 }, { width: 390, height: 844 }]) {
    await page.setViewportSize(viewport);
    await page.waitForTimeout(300);
    if (viewport.width < 768) {
      await ada.setOffline(true);
      await expect(page.getByRole("status").filter({ hasText: "You are offline." })).toBeVisible();
    }
    toast = await raiseToast(page);
    const field = (await composer.boundingBox())!;
    expect(overlaps(toast, field), `the toast covers the composer at ${viewport.width} px`).toBe(false);
    expect(toast.y, "the toast is on screen").toBeGreaterThanOrEqual(0);
    if (viewport.width < 768) expect(toast.y + toast.height, "the toast sits above the composer").toBeLessThanOrEqual(field.y);
  }
  await ada.setOffline(false);
  await ada.request.post(`/api/api/v1/groups/${encodeURIComponent(group)}/leave`, { headers: sameOrigin() });
  expect(errors).toEqual([]);
  await ada.close();
  await bo.close();
});
