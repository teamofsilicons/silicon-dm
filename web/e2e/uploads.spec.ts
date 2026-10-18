/**
 * Attachments of any size from every entry point, end to end against the local chat stack (Silicon Accounts, dm-api,
 * the Briefcase stand-in): several files from the + menu; a paste of several files (and text that stays text); a drop
 * of several files, of a web page's picture (attached as a link) and of a folder (it asks above 20 files); a ~300 MB
 * file prepared (its SHA-256 in the browser), streamed through this website into Briefcase and seen by the other
 * Carbon; cancelling mid-upload, and closing the tab, both leave Briefcase's reservation cancelled. Big files are
 * sparse (no disk), each of a size of its own (so its digest is this run's alone); the stand-in's copy is removed after.
 * Each test makes its own group and asserts only on what it made.
 */
import { createHash } from "node:crypto";
import { closeSync, ftruncateSync, mkdirSync, openSync, rmSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { bytesWords } from "../lib/upload/progress";
import { accountsSession, runAccounts, sameOrigin, signIn, watchErrors } from "./support";

const BRIEFCASE = process.env.E2E_BRIEFCASE_URL ?? "http://127.0.0.1:4541";
const MB = 1024 * 1024;
/** A 12 × 8 PNG. */
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAwAAAAICAIAAABChommAAAAPklEQVR4nGP4taqCIGJ4vqmPIGK4umsJQcRw+NAugohh46kLWNH/az1ABGEzuOZOJYgYrBM6CCIGw+BqgggAm7etGSu/h7MAAAAASUVORK5CYII=", "base64");
const PDF = Buffer.from("%PDF-1.4\n%%EOF\n");

async function hydrated(page: Page): Promise<void> {
  await page.waitForFunction(() => {
    const field = document.querySelector("[data-composer-field]");
    return !!field && Object.keys(field).some(key => key.startsWith("__reactProps"));
  });
}

const apiHeaders = () => ({ ...sameOrigin(), "x-dm-contract-version": "4", "content-type": "application/json" });

/** Ada and Bo signed in, in a new group of their own. */
async function twoCarbons(browser: Browser, stamp: string): Promise<{ page: Page; friend: Page; group: string; close: () => Promise<void> }> {
  const carbon = await accountsSession(browser, "carbon");
  const other = await accountsSession(browser, "friend");
  const page = await carbon.newPage();
  const friend = await other.newPage();
  await signIn(page, runAccounts().carbon.email);
  await signIn(friend, runAccounts().friend.email);
  const created = await page.request.post("/api/api/v1/groups", { headers: { ...apiHeaders(), "idempotency-key": `uploads-${stamp}` }, data: { type: "create_group", data: { name: `Uploads ${stamp}`, member_ids: [runAccounts().friend.id] } } });
  expect(created.status(), await created.text()).toBe(201);
  const group = ((await created.json()) as { data: { id: string } }).data.id;
  const accepted = await friend.request.post(`/api/api/v1/groups/${encodeURIComponent(group)}/accept`, { headers: apiHeaders() });
  expect(accepted.ok(), await accepted.text()).toBeTruthy();
  await page.goto(`/c/${encodeURIComponent(group)}`);
  await expect(page.getByRole("textbox", { name: /^Message Uploads/ })).toBeVisible();
  await hydrated(page);
  return { page, friend, group, close: async () => { await carbon.close(); await other.close(); } };
}

const tray = (page: Page) => page.getByRole("list", { name: "Going with this message" });
const article = (page: Page, text: string) => page.locator("article[data-message-code]", { hasText: text });

/** A file of `size` zero bytes that takes no disk (sparse), and its SHA-256. */
function sparseFile(name: string, size: number): { path: string; sha256: string } {
  const directory = join(test.info().outputDir, "files");
  mkdirSync(directory, { recursive: true });
  const path = join(directory, name);
  const handle = openSync(path, "w");
  ftruncateSync(handle, size);
  closeSync(handle);
  const hash = createHash("sha256");
  const block = Buffer.alloc(8 * MB);
  for (let left = size; left > 0; left -= block.length) hash.update(left >= block.length ? block : block.subarray(0, left));
  return { path, sha256: hash.digest("hex") };
}

/** A size of this run's own (the digest of its zeros is then this run's alone at the shared stand-in). */
const ownSize = (base: number) => base + Math.floor(Math.random() * 4 * MB);

/** The stand-in's record of an upload by the name it was saved under (it ends with the file's own name). */
async function standInUploadNamed(name: string): Promise<{ state: string; size: number } | null> {
  const state = (await (await fetch(`${BRIEFCASE}/_state`)).json()) as { uploads: Array<{ name: string; state: string; size: number; created_at: string }> };
  return state.uploads.filter(upload => upload.name.endsWith(`-${name}`)).sort((a, b) => a.created_at.localeCompare(b.created_at)).at(-1) ?? null;
}

/** Makes the stand-in hold its answer to one upload's bytes (by a name only this test uses) for `ms`: Briefcase storing a large file. */
async function slowToStore(name: string, ms: number): Promise<void> {
  const response = await fetch(`${BRIEFCASE}/_faults`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ route: "transfer", match: name, delay_ms: ms }) });
  expect(response.status, await response.text()).toBe(201);
}

/** The stand-in's record of an upload, by its digest. */
async function standInUpload(sha256: string): Promise<{ state: string; size: number } | null> {
  const state = (await (await fetch(`${BRIEFCASE}/_state`)).json()) as { uploads: Array<{ sha256: string; state: string; size: number; created_at: string }> };
  const mine = state.uploads.filter(upload => upload.sha256 === sha256).sort((a, b) => a.created_at.localeCompare(b.created_at));
  return mine.at(-1) ?? null;
}

/** Slows this page's uploads (Chromium's own throttling applies to request bodies). */
async function throttleUploads(page: Page, bytesPerSecond: number): Promise<void> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Network.enable");
  await cdp.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: bytesPerSecond });
}

test("the + menu takes several files at once: each uploads with its progress, they go with the message, and the other Carbon sees them", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, friend, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  await page.getByRole("button", { name: "Add a file, a link or a GIF" }).click();
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("menuitem", { name: /Upload a file/ }).click();
  await (await chooser).setFiles([
    { name: `harbour-${stamp}.png`, mimeType: "image/png", buffer: PNG },
    { name: `plan-${stamp}.pdf`, mimeType: "application/pdf", buffer: PDF },
    { name: `notes-${stamp}.txt`, mimeType: "text/plain", buffer: Buffer.from(`Notes ${stamp}\n`) },
  ]);
  await expect(tray(page).locator("[data-tray-item]")).toHaveCount(3);
  await expect(tray(page).locator('[data-tray-item][data-status="ready"]')).toHaveCount(3, { timeout: 30_000 });
  await expect(tray(page).getByText("PDF · ", { exact: false })).toBeVisible();
  const field = page.getByRole("textbox", { name: /^Message / });
  await field.fill(`Three files ${stamp}`);
  await field.press("Enter");
  const mine = article(page, `Three files ${stamp}`);
  await expect(mine).toBeVisible({ timeout: 30_000 });
  await friend.goto(page.url());
  const theirs = article(friend, `Three files ${stamp}`);
  await expect(theirs).toBeVisible({ timeout: 30_000 });
  await expect(theirs.getByText(`plan-${stamp}.pdf`)).toBeVisible();
  await expect(theirs.getByText(`notes-${stamp}.txt`)).toBeVisible();
  const picture = theirs.locator('img[src^="/api/uploads/file?url="]').first();
  await expect.poll(() => picture.evaluate((node: HTMLImageElement) => (node.complete ? node.naturalWidth : 0))).toBe(12);
  expect(errors).toEqual([]);
  await close();
});

test("paste: several files at once land in the tray; text, or files with other text (cells from a sheet), stays a paste of text", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  const field = page.getByRole("textbox", { name: /^Message / });
  await field.click();
  const paste = (files: Array<{ name: string; type: string; text?: string }>, text: string) =>
    page.evaluate(
      ({ files, text, png }) => {
        const data = new DataTransfer();
        // A picture's bytes are a real picture (a screenshot), anything else its own words.
        for (const file of files) data.items.add(new File([file.text ?? Uint8Array.from(atob(png), character => character.charCodeAt(0))], file.name, { type: file.type }));
        if (text) data.setData("text/plain", text);
        const event = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
        document.querySelector("[data-composer-field]")!.dispatchEvent(event);
        return event.defaultPrevented;
      },
      { files, text, png: PNG.toString("base64") },
    );
  // Two files copied in the Finder: the files, and their names as text.
  expect(await paste([{ name: `brief-${stamp}.pdf`, type: "application/pdf", text: "%PDF-1.4" }, { name: "image.png", type: "image/png" }], `brief-${stamp}.pdf\nimage.png`)).toBe(true);
  await expect(tray(page).getByText(`brief-${stamp}.pdf`, { exact: true })).toBeVisible();
  await expect(tray(page).getByText(/^Pasted image \d{4}-\d{2}-\d{2} at \d{2}\.\d{2}\.\d{2}\.png$/)).toBeVisible();
  // Words are pasted as words; cells copied from a sheet (text and a picture of them) too.
  expect(await paste([], `Hello ${stamp}`)).toBe(false);
  expect(await paste([{ name: "image.png", type: "image/png" }], "Q3\t412\nQ4\t508")).toBe(false);
  await expect(tray(page).locator("[data-tray-item]")).toHaveCount(2);
  await expect(tray(page).locator('[data-tray-item][data-status="ready"]')).toHaveCount(2, { timeout: 30_000 });
  expect(errors).toEqual([]);
  await close();
});

test("drop: several files, a web page's picture (attached as its https link, never fetched), and a folder (it asks above 20 files)", async ({ browser }) => {
  const stamp = Date.now().toString(36);
  const { page, group, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  // This page is the one in use (the other Carbon's opened after it).
  await page.bringToFront();
  const layer = page.locator("[data-drop-layer]");
  /**
   * Drags what a `kind` of drop carries over the conversation and drops it on the drop layer. The DataTransfer is made in
   * the page; a folder's entries are faked (a page cannot make a real folder), the rest is the browser's own.
   */
  const drop = async (kind: "files" | "link" | "folder", check: () => Promise<void>) => {
    await page.evaluate(
      ({ kind, stamp }) => {
        let data: unknown;
        if (kind === "files") {
          const transfer = new DataTransfer();
          transfer.items.add(new File(["a"], `one-${stamp}.txt`, { type: "text/plain" }));
          transfer.items.add(new File(["b"], `two-${stamp}.txt`, { type: "text/plain" }));
          data = transfer;
        } else if (kind === "link") {
          const transfer = new DataTransfer();
          transfer.setData("text/uri-list", "https://example.com/gallery");
          transfer.setData("text/html", `<a href="https://example.com/gallery"><img src="https://images.example.com/cat-${stamp}.png"></a>`);
          data = transfer;
        } else {
          const entry = (name: string) => ({ isFile: true, isDirectory: false, name, file: (done: (file: File) => void) => done(new File([name], name, { type: "text/plain" })) });
          const children = [...Array.from({ length: 25 }, (_, index) => entry(`scan-${stamp}-${String(index + 1).padStart(2, "0")}.txt`)), entry(".DS_Store")];
          const folder = { isFile: false, isDirectory: true, name: `Scans ${stamp}`, createReader: () => { let given = false; return { readEntries: (done: (entries: unknown[]) => void) => { done(given ? [] : children); given = true; } }; } };
          data = { types: ["Files"], files: [], items: [{ kind: "file", getAsFile: () => null, webkitGetAsEntry: () => folder }], getData: () => "" };
        }
        const main = document.getElementById("main")!;
        const scope = window as unknown as { __drop: unknown; __over: number };
        scope.__drop = data;
        const fire = (type: string, target: Element) => {
          const event = new Event(type, { bubbles: true, cancelable: true });
          Object.defineProperty(event, "dataTransfer", { value: data });
          target.dispatchEvent(event);
        };
        fire("dragenter", main);
        scope.__over = window.setInterval(() => fire("dragover", main), 60);
      },
      { kind, stamp },
    );
    await check();
    await page.evaluate(() => {
      const scope = window as unknown as { __drop: unknown; __over: number };
      window.clearInterval(scope.__over);
      const target = document.querySelector("[data-drop-layer] button") ?? document.querySelector("[data-drop-layer]")!;
      const event = new Event("drop", { bubbles: true, cancelable: true });
      Object.defineProperty(event, "dataTransfer", { value: scope.__drop });
      target.dispatchEvent(event);
    });
    await expect(layer).toHaveCount(0);
  };

  // The note names the website's own limit when its operator set one (UPLOADS_MAX_BYTES), else none.
  const config = (await (await page.request.get("/api/uploads/config")).json()) as { max_bytes: number; capped?: boolean };
  const note = config.capped ? `Up to ${bytesWords(config.max_bytes)} each, folders too` : "Any size, folders too";
  await drop("files", async () => {
    await expect(layer.getByText("Drop files to send them")).toBeVisible();
    await expect(layer.getByText(note)).toBeVisible();
  });
  await expect(tray(page).getByText(`one-${stamp}.txt`, { exact: true })).toBeVisible();
  await expect(tray(page).getByText(`two-${stamp}.txt`, { exact: true })).toBeVisible();

  await drop("link", async () => {
    await expect(layer.getByText("Drop the link to attach it")).toBeVisible();
  });
  await expect(tray(page).locator('[data-kind="link"]').getByText("images.example.com")).toBeVisible();

  // A folder of 25 files and a hidden one: it asks first, then takes the 25.
  await drop("folder", async () => {
    await expect(layer.getByText("Drop files to send them")).toBeVisible();
  });
  const question = page.getByRole("group", { name: "Upload the dropped folder's files?" });
  await expect(question.getByText(`Scans ${stamp} has 25 files. Upload all of them?`)).toBeVisible();
  await expect(question.getByRole("button", { name: "Upload 25 files" })).toBeFocused();
  await question.getByRole("button", { name: "Upload 25 files" }).click();
  await expect(tray(page).locator('[data-kind="file"]')).toHaveCount(27);
  await expect(tray(page).locator('[data-kind="file"][data-status="ready"]')).toHaveCount(27, { timeout: 60_000 });

  const field = page.getByRole("textbox", { name: /^Message / });
  await field.fill(`Dropped ${stamp}`);
  await field.press("Enter");
  const mine = article(page, `Dropped ${stamp}`);
  await expect(mine).toBeVisible({ timeout: 30_000 });
  await expect(mine.getByText(`scan-${stamp}-25.txt`)).toBeVisible();
  const stored = await page.request.get(`/api/api/v1/conversations/${encodeURIComponent(group)}/messages?limit=5`, { headers: apiHeaders() });
  const sent = ((await stored.json()) as { data: { items: Array<{ message: string; attachments: string[] }> } }).data.items.find(message => message.message === `Dropped ${stamp}`);
  expect(sent?.attachments).toContain(`https://images.example.com/cat-${stamp}.png`);
  expect(sent?.attachments.length).toBe(28);
  expect(errors).toEqual([]);
  await close();
});

test("a ~300 MB file: prepared in the browser, streamed through this website into Briefcase, and seen by the other Carbon", async ({ browser }) => {
  test.setTimeout(240_000);
  const stamp = Date.now().toString(36);
  const { page, friend, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  const config = (await (await page.request.get("/api/uploads/config")).json()) as { max_bytes: number; capped?: boolean };
  const size = ownSize(300 * MB);
  const name = `rushes-${stamp}.bin`;
  const { path, sha256 } = sparseFile(name, size);
  try {
    if (config.capped && config.max_bytes < size) {
      // This website was started with a limit of its own (UPLOADS_MAX_BYTES): the tray says so before any byte moves.
      await page.locator("input[data-composer-file]").setInputFiles(path);
      await expect(tray(page).getByText(/the most this DM website takes/)).toBeVisible();
      test.info().annotations.push({ type: "limit", description: `UPLOADS_MAX_BYTES=${config.max_bytes} here: the 300 MB journey needs a website without a limit of its own` });
      return;
    }
    expect(config.max_bytes).toBeGreaterThanOrEqual(5 * 1024 ** 4 - 1);
    await page.locator("input[data-composer-file]").setInputFiles(path);
    const item = tray(page).locator("[data-tray-item]").first();
    await expect(item.getByText(/^Preparing… \d+%$/)).toBeVisible();
    await expect(item).toHaveAttribute("data-status", "ready", { timeout: 180_000 });
    await expect(item.getByText(/^BIN · 30\d MB$/)).toBeVisible();
    expect(await standInUpload(sha256)).toMatchObject({ state: "committed", size });
    const field = page.getByRole("textbox", { name: /^Message / });
    await field.fill(`Rushes ${stamp}`);
    await field.press("Enter");
    await expect(article(page, `Rushes ${stamp}`)).toBeVisible({ timeout: 30_000 });
    await friend.goto(page.url());
    const theirs = article(friend, `Rushes ${stamp}`);
    await expect(theirs).toBeVisible({ timeout: 30_000 });
    const download = theirs.getByRole("link", { name: `Download ${name}` });
    await expect(download).toBeVisible();
    const head = await friend.request.head((await download.getAttribute("href"))!);
    expect(head.status()).toBe(200);
    expect(Number(head.headers()["content-length"])).toBe(size);
    expect(errors).toEqual([]);
  } finally {
    rmSync(path, { force: true });
    await fetch(`${BRIEFCASE}/_files/${sha256}`, { method: "DELETE" }).catch(() => undefined);
    await close();
  }
});

test("cancelling mid-upload stops the transfer and Briefcase's reservation is cancelled; the tray says how far it was", async ({ browser }) => {
  test.setTimeout(180_000);
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  const size = ownSize(90 * MB);
  const name = `archive-${stamp}.zip`;
  const { path, sha256 } = sparseFile(name, size);
  try {
    await throttleUploads(page, 8 * MB);
    await page.locator("input[data-composer-file]").setInputFiles(path);
    const item = tray(page).locator("[data-tray-item]").first();
    // Bytes of the whole, then the speed and the time left once two seconds of sending are seen.
    await expect(item.getByText(/^\d+(\.\d)? of 9\d MB · \d+(\.\d)? MB\/s · \d+ s left$/)).toBeVisible({ timeout: 60_000 });
    await expect.poll(async () => (await standInUpload(sha256))?.state).toBe("receiving");
    await page.getByRole("button", { name: `Cancel ${name}` }).click();
    await expect(tray(page)).toHaveCount(0);
    await expect.poll(async () => (await standInUpload(sha256))?.state, { timeout: 20_000 }).toBe("cancelled");
    expect(errors).toEqual([]);
  } finally {
    rmSync(path, { force: true });
    await close();
  }
});

test("leaving the page while a file uploads asks first; closing the tab cancels Briefcase's reservation", async ({ browser }) => {
  test.setTimeout(180_000);
  const stamp = Date.now().toString(36);
  const { page, close } = await twoCarbons(browser, stamp);
  const size = ownSize(96 * MB);
  const { path, sha256 } = sparseFile(`leaving-${stamp}.bin`, size);
  try {
    await throttleUploads(page, 8 * MB);
    // Browsers ask before leaving only a page the person has used: a click first.
    await page.getByRole("textbox", { name: /^Message / }).click();
    await page.locator("input[data-composer-file]").setInputFiles(path);
    await expect.poll(async () => (await standInUpload(sha256))?.state, { timeout: 60_000 }).toBe("receiving");
    const asked = page.waitForEvent("dialog");
    await page.close({ runBeforeUnload: true });
    const dialog = await asked;
    expect(dialog.type()).toBe("beforeunload");
    await dialog.accept();
    await expect.poll(async () => (await standInUpload(sha256))?.state, { timeout: 20_000 }).toBe("cancelled");
  } finally {
    rmSync(path, { force: true });
    await close();
  }
});

test("a file Briefcase takes minutes to store: the website answers before an ingress would cut it, the page asks until it is saved, and one cancelled meanwhile is cancelled at Briefcase", async ({ browser }) => {
  test.setTimeout(150_000);
  const stamp = Date.now().toString(36);
  const { page, friend, close } = await twoCarbons(browser, stamp);
  const errors = watchErrors(page);
  const kept = `storing-${stamp}.pdf`;
  const dropped = `abandoned-${stamp}.pdf`;
  // After the last byte, the stand-in holds its answer past the website's 20 s hold (only these two uploads').
  await slowToStore(kept, 30_000);
  await slowToStore(dropped, 90_000);
  const asked = page.waitForRequest(request => request.url().includes("/api/uploads/saving"), { timeout: 60_000 });
  await page.locator("input[data-composer-file]").setInputFiles([
    { name: kept, mimeType: "application/pdf", buffer: Buffer.from(`%PDF-1.4 ${stamp} kept\n`) },
    { name: dropped, mimeType: "application/pdf", buffer: Buffer.from(`%PDF-1.4 ${stamp} dropped\n`) },
  ]);
  await asked;
  const keptItem = tray(page).locator("[data-tray-item]", { hasText: kept });
  await expect(keptItem.getByText("Saving… this can take a few minutes")).toBeVisible({ timeout: 15_000 });
  await expect(tray(page).locator("[data-tray-item]", { hasText: dropped }).getByText("Saving… this can take a few minutes")).toBeVisible({ timeout: 15_000 });
  await page.getByRole("button", { name: `Cancel ${dropped}` }).click();
  await expect.poll(async () => (await standInUploadNamed(dropped))?.state, { timeout: 20_000 }).toBe("cancelled");
  await expect(keptItem).toHaveAttribute("data-status", "ready", { timeout: 40_000 });
  expect((await standInUploadNamed(kept))?.state).toBe("committed");
  const field = page.getByRole("textbox", { name: /^Message / });
  await field.fill(`Stored ${stamp}`);
  await field.press("Enter");
  await expect(article(page, `Stored ${stamp}`)).toBeVisible({ timeout: 30_000 });
  await friend.goto(page.url());
  const theirs = article(friend, `Stored ${stamp}`);
  await expect(theirs.getByText(kept)).toBeVisible({ timeout: 30_000 });
  await expect(theirs.getByText(dropped)).toHaveCount(0);
  expect(errors).toEqual([]);
  await close();
});
