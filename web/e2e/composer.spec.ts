/**
 * The composer end to end against the local chat stack (Silicon Accounts, dm-api, the Briefcase and Giphy stand-ins):
 * a file uploaded into the Carbon's Silicon Briefcase with its progress in the tray and sent while it still uploads,
 * which the other Carbon sees through this website's file proxy; a waiting message that still goes after another
 * conversation was opened; offline uploads that wait for the connection; your own HTML file served only through the
 * proxy; a pasted picture; a dropped file; a GIF picked with the keyboard; slash commands; Add a link. Each run makes its
 * own groups and asserts only on what it sent.
 */
import { join } from "node:path";
import { expect, test, type Browser, type BrowserContextOptions, type Page } from "@playwright/test";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

/** A 12 × 8 PNG (a dusk sky over the sea). */
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAwAAAAICAIAAABChommAAAAPklEQVR4nGP4taqCIGJ4vqmPIGK4umsJQcRw+NAugohh46kLWNH/az1ABGEzuOZOJYgYrBM6CCIGw+BqgggAm7etGSu/h7MAAAAASUVORK5CYII=", "base64");
const mod = process.platform === "darwin" ? "Meta" : "Control";

/** The composer is React's (hydrated), not only the server's HTML: keys typed before that would be lost. */
async function hydrated(page: Page): Promise<void> {
  await page.waitForFunction(() => {
    const field = document.querySelector("[data-composer-field]");
    return !!field && Object.keys(field).some(key => key.startsWith("__reactProps"));
  });
}

const apiHeaders = () => ({ ...sameOrigin(), "x-dm-contract-version": "4", "content-type": "application/json" });

/** A new group of Ada's with Bo in it (made through this website's own API proxy). */
async function newGroup(page: Page, friend: Page, name: string): Promise<string> {
  const created = await page.request.post("/api/api/v1/groups", { headers: { ...apiHeaders(), "idempotency-key": `composer-${name.replace(/\W+/g, "-")}` }, data: { type: "create_group", data: { name, member_ids: [runAccounts().friend.id] } } });
  expect(created.status(), await created.text()).toBe(201);
  const group = ((await created.json()) as { data: { id: string } }).data.id;
  const accepted = await friend.request.post(`/api/api/v1/groups/${encodeURIComponent(group)}/accept`, { headers: apiHeaders() });
  expect(accepted.ok(), await accepted.text()).toBeTruthy();
  return group;
}

/** The newest messages of a conversation, as the service has them. */
async function storedMessages(page: Page, address: string): Promise<Array<{ message: string; attachments: string[] }>> {
  const answer = await page.request.get(`/api/api/v1/conversations/${encodeURIComponent(address)}/messages?limit=10`, { headers: apiHeaders() });
  expect(answer.ok(), await answer.text()).toBeTruthy();
  return ((await answer.json()) as { data: { items: Array<{ message: string; attachments: string[] }> } }).data.items;
}

/** Ada and Bo signed in, in a new group of their own (made through this website's own API proxy). */
async function twoCarbons(browser: Browser, stamp: string, options: BrowserContextOptions = {}): Promise<{ page: Page; friend: Page; group: string; close: () => Promise<void> }> {
  const carbon = await accountsSession(browser, "carbon", options);
  const other = await accountsSession(browser, "friend", options);
  const page = await carbon.newPage();
  const friend = await other.newPage();
  await signIn(page, runAccounts().carbon.email);
  await signIn(friend, runAccounts().friend.email);
  const group = await newGroup(page, friend, `Composer check ${stamp}`);
  await page.goto(`/c/${encodeURIComponent(group)}`);
  await expect(page.getByRole("textbox", { name: /^Message Composer check/ })).toBeVisible();
  await hydrated(page);
  return { page, friend, group, close: async () => { await carbon.close(); await other.close(); } };
}

const tray = (page: Page) => page.getByRole("list", { name: "Going with this message" });
const article = (page: Page, text: string) => page.locator("article[data-message-code]", { hasText: text });

test("a file goes into Silicon Briefcase with its progress, sends when ready, and the other Carbon sees it through this website", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, friend, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  const field = page.getByRole("textbox", { name: /^Message / });
  // Hold the bytes a moment on their way, so the tray shows the upload under way.
  let release: () => void = () => undefined;
  const held = new Promise<void>(resolve => (release = resolve));
  await page.route(url => url.pathname === "/api/uploads" && url.searchParams.has("name"), async route => {
    await held;
    await route.continue();
  });
  const name = `harbour-${stamp}.png`;
  await page.locator("input[data-composer-file]").setInputFiles({ name, mimeType: "image/png", buffer: PNG });
  await expect(tray(page).getByRole("progressbar", { name })).toBeVisible();
  await expect(tray(page).getByText(/^Uploading \d+%$/)).toBeVisible();

  // Enter while it uploads: the message waits for it (and says so), then goes by itself.
  await field.fill(`Harbour photo ${stamp}`);
  await field.press("Enter");
  await expect(page.getByRole("status").filter({ hasText: `Sends when ${name} is uploaded.` })).toBeVisible();
  await expect(article(page, `Harbour photo ${stamp}`)).toHaveCount(0);
  release();
  await expect(article(page, `Harbour photo ${stamp}`)).toBeVisible({ timeout: 30_000 });
  await expect(tray(page)).toHaveCount(0);
  await expect(field).toHaveValue("");

  // Bo sees the picture through this website's proxy (Briefcase serves its bytes to its own origin only).
  await friend.goto(page.url());
  await hydrated(friend);
  const theirs = article(friend, `Harbour photo ${stamp}`);
  await expect(theirs).toBeVisible();
  const image = theirs.locator('img[src^="/api/uploads/file?url="]').first();
  await expect(image).toBeVisible();
  await expect.poll(() => image.evaluate((node: HTMLImageElement) => (node.complete ? node.naturalWidth : 0))).toBe(12);
  const bytes = await friend.request.get((await image.getAttribute("src"))!);
  expect(bytes.status()).toBe(200);
  expect(bytes.headers()["content-type"]).toBe("image/png");
  expect(bytes.headers()["x-content-type-options"]).toBe("nosniff");
  expect(bytes.headers()["content-security-policy"]).toBe("sandbox; default-src 'none'");
  expect(bytes.headers()["content-disposition"]).toMatch(new RegExp(`^inline; filename="${name}"`));
  expect(Buffer.from(await bytes.body()).equals(PNG)).toBe(true);
  // The proxy is no open door: another host is refused.
  const other = await friend.request.get(`/api/uploads/file?url=${encodeURIComponent("https://example.com/api/v1/public/x/y.png")}`);
  expect(other.status()).toBe(400);
  expect(errors).toEqual([]);
  await close();
});

test("a pasted picture and a dropped file land in the tray and go with the message", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  const field = page.getByRole("textbox", { name: /^Message / });
  await field.click();

  // A screenshot pasted from the clipboard (named "image.png" there) gets a name of its own.
  await page.evaluate(base64 => {
    const data = new DataTransfer();
    data.items.add(new File([Uint8Array.from(atob(base64), character => character.charCodeAt(0))], "image.png", { type: "image/png" }));
    document.querySelector("[data-composer-field]")!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  }, PNG.toString("base64"));
  await expect(tray(page).getByText(/^Pasted image \d{4}-\d{2}-\d{2} at \d{2}\.\d{2}\.\d{2}\.png$/)).toBeVisible();
  await expect(tray(page).getByText("PNG image", { exact: false })).toBeVisible({ timeout: 20_000 });

  // A file dragged over the conversation brings the drop layer; dropping it on the target adds it once.
  await page.evaluate(() => {
    const data = new DataTransfer();
    data.items.add(new File(["Notes from the call\n"], "call-notes.txt", { type: "text/plain" }));
    const main = document.getElementById("main")!;
    const scope = window as unknown as { __drop: DataTransfer; __over: number };
    scope.__drop = data;
    main.dispatchEvent(new DragEvent("dragenter", { bubbles: true, cancelable: true, dataTransfer: data }));
    scope.__over = window.setInterval(() => main.dispatchEvent(new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: data })), 60);
  });
  const layer = page.locator("[data-drop-layer]");
  await expect(layer.getByText("Drop files to send them")).toBeVisible();
  await page.evaluate(() => {
    const scope = window as unknown as { __drop: DataTransfer; __over: number };
    window.clearInterval(scope.__over);
    const target = document.querySelector("[data-drop-layer] button") ?? document.querySelector("[data-drop-layer]")!;
    target.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: scope.__drop }));
  });
  await expect(layer).toHaveCount(0);
  await expect(tray(page).getByText("call-notes.txt", { exact: true })).toHaveCount(1);
  await expect(tray(page).getByText("TXT", { exact: false })).toBeVisible({ timeout: 20_000 });

  await field.fill(`Pasted and dropped ${stamp}`);
  await field.press("Enter");
  const mine = article(page, `Pasted and dropped ${stamp}`);
  await expect(mine).toBeVisible({ timeout: 30_000 });
  await expect(mine.getByText("call-notes.txt")).toBeVisible();
  await expect(mine.locator(`img[src^="blob:"], img[src^="/api/uploads/file"]`).first()).toBeVisible();
  expect(errors).toEqual([]);
  await close();
});

test("slash commands: /gif finds GIFs the keyboard picks, /link adds a link, /upload and ⌘⇧U open the files, Esc sends the text", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  const field = page.getByRole("textbox", { name: /^Message / });
  await field.click();

  // "/" lists the commands; the arrows move through them while the keyboard stays in the field.
  await field.fill("/");
  const commands = page.getByRole("listbox", { name: "Commands" });
  await expect(commands.getByRole("option")).toHaveCount(5);
  await expect(commands.getByRole("option", { selected: true })).toContainText("/gif");
  await field.press("ArrowDown");
  await expect(commands.getByRole("option", { selected: true })).toContainText("/voice");
  await expect(field).toHaveAttribute("aria-activedescendant", /voice$/);

  // /gif wave: the picker opens with the search done; ↓ into the grid, Enter picks, Enter sends.
  await field.fill("/gif wave");
  await field.press("Enter");
  const picker = page.getByRole("dialog", { name: "Choose a GIF" });
  await expect(picker).toBeVisible();
  await expect(picker.getByRole("searchbox", { name: "Search GIFs" })).toHaveValue("wave");
  await expect(picker.getByText("Powered by GIPHY")).toBeVisible();
  const wave = picker.getByRole("button", { name: "Wave hello GIF" });
  await expect(wave).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await expect(wave).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(picker).toHaveCount(0);
  await expect(field).toBeFocused();
  await expect(tray(page).getByText("Wave hello GIF")).toBeVisible();
  await page.keyboard.type(`A wave ${stamp}`);
  await page.keyboard.press("Enter");
  await expect(article(page, `A wave ${stamp}`)).toBeVisible({ timeout: 30_000 });

  // Esc closes the picker and gives the keyboard straight back to the field.
  await field.fill("/gif");
  await field.press("Enter");
  await expect(picker).toBeVisible();
  await expect(picker.getByRole("searchbox", { name: "Search GIFs" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(picker).toHaveCount(0);
  await expect(field).toBeFocused();

  // /link with an address adds it at once.
  await field.fill("/link example.com/notes");
  await field.press("Enter");
  await expect(tray(page).getByText("example.com")).toBeVisible();
  await field.fill(`Notes link ${stamp}`);
  await field.press("Enter");
  await expect(article(page, `Notes link ${stamp}`).locator('a[href="https://example.com/notes"]').first()).toBeVisible({ timeout: 30_000 });

  // /upload (Tab runs it) and ⌘⇧U open the file chooser.
  await field.fill("/upl");
  const chooser = page.waitForEvent("filechooser");
  await field.press("Tab");
  await (await chooser).setFiles({ name: `plan-${stamp}.pdf`, mimeType: "application/pdf", buffer: Buffer.from("%PDF-1.4\n%%EOF\n") });
  await expect(tray(page).getByText(`plan-${stamp}.pdf`)).toBeVisible();
  await expect(field).toHaveValue("", { timeout: 5_000 });
  const again = page.waitForEvent("filechooser");
  await field.press(`${mod}+Shift+U`);
  await (await again).setFiles([]);
  await page.getByRole("button", { name: `Remove plan-${stamp}.pdf` }).click();
  await expect(tray(page)).toHaveCount(0);

  // Esc closes the list and leaves the text: Enter then sends it as it is.
  await field.fill(`/gif ${stamp}`);
  await expect(commands).toBeVisible();
  await field.press("Escape");
  await expect(commands).toHaveCount(0);
  await expect(field).toHaveValue(`/gif ${stamp}`);
  await field.press("Enter");
  await expect(article(page, `/gif ${stamp}`)).toBeVisible({ timeout: 30_000 });
  expect(errors).toEqual([]);
  await close();
});

test("Add a link from the + menu: one field, https only, Esc gives the keyboard back", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp);
  const field = page.getByRole("textbox", { name: /^Message / });
  await page.getByRole("button", { name: "Add a file, a link or a GIF" }).click();
  const menu = page.getByRole("menu");
  await expect(menu.getByRole("menuitem", { name: /Upload a file/ })).toBeVisible();
  await menu.getByRole("menuitem", { name: "Add a link" }).click();
  const link = page.getByRole("textbox", { name: "Link to add" });
  await expect(link).toBeFocused();
  await link.fill("ftp://files.example.com/x");
  await link.press("Enter");
  await expect(page.getByRole("alert").filter({ hasText: "That is not an https link." })).toBeVisible();
  await link.press("Escape");
  await expect(link).toHaveCount(0);
  await expect(field).toBeFocused();
  await page.getByRole("button", { name: "Add a file, a link or a GIF" }).click();
  await page.getByRole("menu").getByRole("menuitem", { name: "Add a link" }).click();
  await expect(link).toBeFocused();
  await link.fill(`docs.example.com/plan-${stamp}`);
  await link.press("Enter");
  await expect(tray(page).getByText("docs.example.com")).toBeVisible();
  await expect(field).toBeFocused();
  await field.fill(`The plan ${stamp}`);
  await field.press("Enter");
  await expect(article(page, `The plan ${stamp}`).locator(`a[href="https://docs.example.com/plan-${stamp}"]`).first()).toBeVisible({ timeout: 30_000 });
  await close();
});

test("a message waiting for its upload still goes when the Carbon opens another conversation meanwhile", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, friend, group, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  const elsewhere = await newGroup(page, friend, `Composer elsewhere ${stamp}`);
  // The list shows the new group from the next load (made through the API, it reaches an open list with its sync).
  await page.reload();
  await hydrated(page);
  let release: () => void = () => undefined;
  const held = new Promise<void>(resolve => (release = resolve));
  await page.route(url => url.pathname === "/api/uploads" && url.searchParams.has("name"), async route => {
    await held;
    await route.continue();
  });
  const field = page.getByRole("textbox", { name: /^Message / });
  await page.locator("input[data-composer-file]").setInputFiles({ name: `away-${stamp}.png`, mimeType: "image/png", buffer: PNG });
  await field.fill(`Sent while away ${stamp}`);
  await field.press("Enter");
  await expect(page.getByRole("status").filter({ hasText: `Sends when away-${stamp}.png is uploaded.` })).toBeVisible();
  // Another conversation, opened from the list (the page stays: the upload and the wish go on).
  await page.getByRole("link", { name: new RegExp(`Composer elsewhere ${stamp}`) }).first().click();
  await expect(page).toHaveURL(url => decodeURIComponent(url.pathname) === `/c/${elsewhere}`);
  await expect(page.getByRole("textbox", { name: `Message Composer elsewhere ${stamp}` })).toBeVisible();
  release();
  await expect.poll(async () => (await storedMessages(page, group)).find(item => item.message === `Sent while away ${stamp}`)?.attachments.length ?? 0, { timeout: 30_000 }).toBe(1);
  // Back in the first conversation: nothing is left in the field or the tray.
  await page.getByRole("link", { name: new RegExp(`Composer check ${stamp}`) }).first().click();
  await expect(article(page, `Sent while away ${stamp}`)).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("textbox", { name: /^Message Composer check/ })).toHaveValue("");
  await expect(tray(page)).toHaveCount(0);
  expect(errors).toEqual([]);
  await close();
});

test("offline, a file waits for the connection, says so, and the message goes when the browser is back", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp);
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(page.getByRole("log")).toBeVisible();
  await page.context().setOffline(true);
  await page.locator("input[data-composer-file]").setInputFiles({ name: `offline-${stamp}.png`, mimeType: "image/png", buffer: PNG });
  await expect(tray(page).getByText("Uploads when online")).toBeVisible();
  await field.fill(`Offline words ${stamp}`);
  await field.press("Enter");
  await expect(page.getByRole("status").filter({ hasText: `You are offline: it sends when you are back and offline-${stamp}.png is uploaded.` })).toBeVisible();
  // Cancel (Esc on a keyboard) takes the wish back; the words and the file stay, and Enter asks again.
  await page.getByRole("button", { name: "Cancel sending" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Not sent: it waits here until you send it." })).toBeVisible();
  await expect(field).toHaveValue(`Offline words ${stamp}`);
  await field.press("Enter");
  await expect(page.getByRole("button", { name: "Cancel sending" })).toBeVisible();
  // The GIF picker says it is offline instead of loading forever.
  await page.getByRole("button", { name: "Add a file, a link or a GIF" }).click();
  await page.getByRole("menu").getByRole("menuitem", { name: "GIF" }).click();
  await expect(page.getByRole("dialog", { name: "Choose a GIF" }).getByText("You are offline. GIFs show here when you are back.")).toBeVisible();
  await page.keyboard.press("Escape");
  await page.context().setOffline(false);
  await expect(article(page, `Offline words ${stamp}`)).toBeVisible({ timeout: 30_000 });
  await expect(tray(page)).toHaveCount(0);
  await close();
});

test("your own HTML file opens through this website's file proxy as a download, never as a page of DM's", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp);
  const field = page.getByRole("textbox", { name: /^Message / });
  await page.locator("input[data-composer-file]").setInputFiles({ name: `page-${stamp}.html`, mimeType: "text/html", buffer: Buffer.from("<!doctype html><title>x</title><script>document.title = 'ran'</script>") });
  await expect(tray(page).getByText("HTML", { exact: false })).toBeVisible({ timeout: 20_000 });
  await field.fill(`A page ${stamp}`);
  await field.press("Enter");
  const mine = article(page, `A page ${stamp}`);
  await expect(mine).toBeVisible({ timeout: 30_000 });
  const hrefs = await mine.locator("a").evaluateAll(nodes => nodes.map(node => node.getAttribute("href") ?? ""));
  expect(hrefs.length).toBeGreaterThan(0);
  for (const href of hrefs) expect(href, "a blob: URL would be a document of DM's own origin").toMatch(/^\/api\/uploads\/file\?url=/);
  const answer = await page.request.get(hrefs[0]!);
  expect(answer.headers()["content-disposition"]).toMatch(/^attachment;/);
  expect(answer.headers()["content-type"]).toBe("application/octet-stream");
  expect(answer.headers()["content-security-policy"]).toBe("sandbox; default-src 'none'");
  expect(answer.headers()["x-content-type-options"]).toBe("nosniff");
  await close();
});

const AXE = join(process.cwd(), "node_modules", "axe-core", "axe.min.js");

/** axe-core's WCAG 2.2 A and AA rules on the composer and what floats from it (the rest of the page is other specs'). */
async function auditComposer(page: Page, label: string): Promise<void> {
  await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== "running"), undefined, { timeout: 5_000 }).catch(() => undefined);
  await page.addScriptTag({ path: AXE });
  const problems = await page.evaluate(async () => {
    const axe = (window as unknown as { axe: { run: (context: object, options: object) => Promise<{ violations: Array<{ id: string; impact: string; help: string; nodes: Array<{ target: string[] }> }> }> } }).axe;
    const include = ["[data-composer]", "[role=menu]", "[role=dialog][aria-label='Choose a GIF']", "[data-drop-layer]"].filter(selector => document.querySelector(selector)).map(selector => [selector]);
    const result = await axe.run({ include }, { runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"] }, resultTypes: ["violations"] });
    return result.violations.map(violation => `${violation.id} (${violation.impact}): ${violation.help} — ${violation.nodes.slice(0, 3).map(node => node.target.join(" ")).join(", ")}`);
  });
  expect(problems, label).toEqual([]);
}

test("the composer passes WCAG 2.2 AA: the tray, the slash list, the + menu and the GIF picker", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp, { bypassCSP: true, reducedMotion: "reduce" });
  const field = page.getByRole("textbox", { name: /^Message / });
  await page.locator("input[data-composer-file]").setInputFiles([
    { name: `look-${stamp}.png`, mimeType: "image/png", buffer: PNG },
    { name: "empty.txt", mimeType: "text/plain", buffer: Buffer.alloc(0) },
  ]);
  await expect(tray(page).getByText("Did not upload: This file is empty", { exact: false })).toBeVisible();
  await expect(tray(page).getByText("PNG image", { exact: false })).toBeVisible({ timeout: 20_000 });
  await field.fill("/link example.com/a");
  await field.press("Enter");
  await field.fill("Words to go with it");
  await auditComposer(page, "the tray");
  await field.fill("/");
  await auditComposer(page, "the slash list");
  await field.press("Escape");
  await field.fill("");
  await page.getByRole("button", { name: "Add a file, a link or a GIF" }).click();
  await auditComposer(page, "the + menu");
  await page.getByRole("menu").getByRole("menuitem", { name: "GIF" }).click();
  const picker = page.getByRole("dialog", { name: "Choose a GIF" });
  await expect(picker.getByRole("button").first()).toBeVisible();
  await auditComposer(page, "the GIF picker");
  await page.keyboard.press("Escape");
  await close();
});
