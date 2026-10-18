/**
 * The shell's promises about what stays in reach. Toasts never cover the message field or its buttons: with the sidebar
 * on screen they rise in its column above the account bar; on a phone, just above the composer (components/chat/shell/
 * toast-place.tsx), checked with the details panel open, at 900 px, at 390 px, and offline. And on a phone, a dialog
 * taller than the screen keeps its actions on it (a sticky footer), and a group renamed in place keeps Save and Cancel
 * inside the details sheet. Each run makes its own group.
 */
import { expect, test, type Page } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

const MOD = process.platform === "darwin" ? "Meta" : "Control";

type Box = { x: number; y: number; width: number; height: number };

/** True when nothing covers the element's middle (the point there is the element or inside it). */
const reachable = (locator: ReturnType<Page["locator"]>) =>
  locator.evaluate(node => {
    const box = node.getBoundingClientRect();
    const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2);
    return !!hit && node.contains(hit);
  });
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

test("on a phone, a tall dialog keeps its actions on screen, and a group renamed in place keeps Save and Cancel in the sheet", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const phone = { width: 390, height: 844 };
  const bo = await accountsSession(browser, "friend", { viewport: phone, isMobile: true, hasTouch: true, deviceScaleFactor: 2 });
  const page = await bo.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().friend.email);

  // New group is taller than a phone with its people to choose from: Cancel and Create group stay on screen, uncovered.
  await page.goto("/c?new=group");
  const dialog = page.getByRole("dialog", { name: "New group" });
  await expect(dialog).toBeVisible();
  for (const name of ["Cancel", "Create group"]) {
    const button = dialog.getByRole("button", { name, exact: true });
    const box = (await button.boundingBox())!;
    expect(box.y + box.height, `${name} is on screen`).toBeLessThanOrEqual(phone.height);
    expect(await reachable(button), `${name} is not covered`).toBe(true);
  }
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);

  // Bo's own group (he is its owner, so he may rename it): Save and Cancel sit inside the sheet, whole.
  const write = { headers: sameOrigin({ "Idempotency-Key": crypto.randomUUID(), "Content-Type": "application/json" }) };
  const created = await bo.request.post("/api/api/v1/groups", { ...write, data: JSON.stringify({ type: "create_group", data: { name: `Layout check ${stamp}`, description: "A long enough description to wrap onto a second line on a phone.", member_ids: [] } }) });
  expect(created.ok(), await created.text()).toBe(true);
  const group = ((await created.json()) as { data: { id: string } }).data.id;
  await page.goto(`/c/${group}`);
  await page.getByRole("button", { name: "Show details" }).click();
  const sheet = page.getByRole("dialog", { name: "Group details" });
  await expect(sheet).toBeVisible();
  await sheet.getByRole("button", { name: /^Group name:/ }).click();
  for (const name of ["Save group name", "Cancel editing"]) {
    const button = sheet.getByRole("button", { name, exact: true });
    await expect(button).toBeVisible();
    const box = (await button.boundingBox())!;
    expect(box.x, `${name} starts inside the screen`).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width, `${name} ends inside the screen`).toBeLessThanOrEqual(phone.width);
    expect(await reachable(button), `${name} is not cut off or covered`).toBe(true);
  }
  // A tap on Cancel cancels (it once landed on the description below it, and leaving the field saved the new name).
  await sheet.getByRole("textbox", { name: "Group name" }).fill(`Not this name ${stamp}`);
  await sheet.getByRole("button", { name: "Cancel editing", exact: true }).tap();
  await expect(sheet.getByRole("button", { name: `Group name: Layout check ${stamp}` })).toBeVisible();
  await page.waitForTimeout(500);
  const after = await bo.request.get(`/api/api/v1/groups/${encodeURIComponent(group)}`, { headers: sameOrigin({ "x-dm-contract-version": "4" }) });
  expect(((await after.json()) as { data: { group: { name: string } } }).data.group.name, "nothing was saved").toBe(`Layout check ${stamp}`);
  expect(errors).toEqual([]);
  await bo.close();
});
