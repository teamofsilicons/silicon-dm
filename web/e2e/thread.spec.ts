/**
 * The thread end to end, as two Carbons (Ada and Bo) use it, against the local chat stack (scripts/chat-stack/up.sh).
 * Every test works in a group of its own run and asserts only on its own messages: other runs share the stack.
 *
 *   live delivery and read: a message reaches the other browser without a reload, turns Read (in the accent) when
 *     read there; the other's typing shows in the header and at the end of the thread; a sender's account card
 *   long threads: a reply quote goes to its original (older pages load), a #code link opens at its message, and older
 *     pages load above without moving what you read
 *   deleting asks in place; an edit keeps its history
 *   the keyboard: ⌘↑ selects the last message, ↑/↓ move, I, R, Esc, and Tab stays inside the selected message
 *   photos: the lightbox by keyboard (Enter, → , Esc back to the photo)
 *   the menu gives the keyboard back (Shift+F10, Esc), keys that do not apply do nothing, code blocks are a Tab stop of
 *     their message only, and a link no older page can bring is answered at once
 *   a custodian's view: only the commands a custodian may run (show, listen --for), never a send
 *   one toolbar at a time, never over another message (beside it; under the message tapped on a phone)
 *   a conversation of several: "3 people"; opening a conversation asks for its draft at most once
 */
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";
import { BASE, watchErrors } from "./support";

const HELPER = join(process.cwd(), "..", "scripts", "chat-stack", "identities.mjs");
const DM_API = `http://127.0.0.1:${process.env.API_PORT || 4521}`;
const MOD = process.platform === "darwin" ? "Meta" : "Control";

test.describe.configure({ mode: "serial" });

/* The stack's identities: a website cookie and a DM token (one sign-in per identity for the whole run). ----------- */

const helper = (...args: string[]) => execFileSync("node", [HELPER, ...args], { encoding: "utf8" });
const tokens = new Map<string, string>();
const tokenOf = (who: string) => {
  if (!tokens.has(who)) tokens.set(who, helper("token", who, "--raw").trim());
  return tokens.get(who)!;
};

/** A photo for made-up https addresses, served by the test (no internet). */
const photo = (label: string, fill: string) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="800" height="600"><rect width="100%" height="100%" fill="${fill}"/><text x="40" y="560" font-family="sans-serif" font-size="48">${label}</text></svg>`;

async function signedIn(browser: Browser, who: string): Promise<BrowserContext> {
  const cookie = JSON.parse(helper("cookie", who)) as { name: string; value: string; httpOnly?: boolean; sameSite?: "Lax" | "Strict" | "None" };
  const context = await browser.newContext({ baseURL: BASE, permissions: ["clipboard-read", "clipboard-write"] });
  await context.addCookies([{ name: cookie.name, value: cookie.value, url: BASE, httpOnly: cookie.httpOnly ?? true, secure: false, sameSite: cookie.sameSite ?? "Lax" }]);
  await context.route(/^https:\/\/img\.example\.test\//, route => {
    const name = new URL(route.request().url()).pathname.split("/").pop() ?? "photo";
    return route.fulfill({ status: 200, contentType: "image/svg+xml", body: photo(name, name.startsWith("a") ? "#C9B79C" : "#9DB4C0") });
  });
  return context;
}

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

interface Sent {
  "message-id": string;
}

const send = (who: string, address: string, data: Record<string, unknown>) =>
  dmApi<Sent>(who, "POST", `/api/v1/conversations/${encodeURIComponent(address)}/messages`, { type: "message.create", data });

const stamp = () => `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;

/** A group of Ada's with Bo in it (accepted), named for this run. */
async function newGroup(label: string): Promise<string> {
  const group = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name: `${label} ${stamp()}`, member_ids: ["c:dmc-bo"] } });
  await dmApi("bo", "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/accept`);
  return group.id;
}

/** What a page reports as broken (e2e/support.ts watchErrors): every error fails the test, hydration mismatches too. */
function threadErrors(page: Page): () => string[] {
  const errors = watchErrors(page);
  return () => errors;
}

/** The toolbars on screen, and anything of another message each one covers (it must cover nothing). */
const toolbars = (page: Page) =>
  page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>("[role='toolbar']")]
      .filter(node => !node.hidden && node.getBoundingClientRect().width > 0)
      .map(node => {
        const row = node.closest("article");
        const box = node.getBoundingClientRect();
        const covered = [...document.querySelectorAll("article[data-message-row]")]
          .filter(other => other !== row)
          .flatMap(other => [...other.querySelectorAll("[data-sq], time, p, button")])
          .filter(element => {
            const rect = element.getBoundingClientRect();
            return rect.width > 0 && rect.left < box.right && box.left < rect.right && rect.top < box.bottom && box.top < rect.bottom;
          }).length;
        return { row: row?.getAttribute("aria-label") ?? "", place: node.dataset.place ?? "", covered };
      }),
  );

const row = (page: Page, text: string) => page.locator("article[data-message-row]", { hasText: text });
const scroller = (page: Page) => page.getByRole("log").locator("xpath=ancestor::div[contains(@class, 'scroller')]");
const focusedLabel = (page: Page) => page.evaluate(() => document.activeElement?.getAttribute("aria-label") ?? document.activeElement?.textContent ?? "");

/* Live delivery, read, typing, the account card ---------------------------------------------------------------------- */

test("a message reaches the other Carbon live, turns Read when read there; typing and account cards", async ({ browser }) => {
  const group = await newGroup("Thread live");
  const ada = await signedIn(browser, "ada");
  const bo = await signedIn(browser, "bo");
  const page = await ada.newPage();
  const friend = await bo.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  await friend.goto(`/c/${group}`);
  // A new conversation says where it begins, once.
  await expect(page.getByText(/^This is the beginning of Thread live/)).toHaveCount(1);

  const words = `Shipping the thread today ${stamp()}`;
  const field = page.getByRole("textbox", { name: /^Message / });
  await field.fill(words);
  await field.press("Enter");
  const mine = row(page, words);
  await expect(mine).toHaveAttribute("data-message-code", /.+/);

  // Bo has it without a reload; once it is on his screen in a focused tab, Ada sees Read, in words for every reader.
  await friend.bringToFront();
  const theirs = row(friend, words);
  await expect(theirs).toBeVisible();
  await expect(theirs).toHaveAttribute("aria-label", new RegExp(`^Message from Ada .* at \\d\\d:\\d\\d: ${words}`));
  await expect(mine).toHaveAttribute("data-status", "read", { timeout: 30_000 });
  await expect(mine).toHaveAttribute("aria-label", /^Your message at \d\d:\d\d, Read: /);
  await expect(mine.locator("[data-state='read']")).toHaveText("Read");

  // Bo's name over his message opens his card (hover, or keyboard focus); it says who he is and offers a conversation.
  await page.bringToFront();
  const reply = `On it ${stamp()}`;
  await send("bo", group, { message: reply });
  const sender = row(page, reply).getByRole("button", { name: "Bo Okafor", exact: true });
  await sender.hover();
  const card = page.getByRole("tooltip");
  await expect(card).toContainText("c:dmc-bo");
  await expect(card).toContainText("Carbon");
  await expect(card.getByRole("button", { name: "Message Bo" })).toBeVisible();
  await page.mouse.move(5, 5);

  // Bo types: Ada's header and the end of her thread say so (presence is read every 15 s).
  await friend.bringToFront();
  const theirField = friend.getByRole("textbox", { name: /^Message / });
  await theirField.click();
  const typing = page.getByText("Bo is typing…").first();
  await expect.poll(async () => {
    await theirField.pressSequentially("a", { delay: 30 });
    return typing.isVisible();
  }, { timeout: 45_000, intervals: [1500] }).toBe(true);
  await expect(page.getByRole("log").getByText("Bo is typing…")).toBeVisible();
  await theirField.fill("");

  expect(errors()).toEqual([]);
  await ada.close();
  await bo.close();
});

/* Long threads: reply jumps, links to a message, older pages ---------------------------------------------------------- */

test("a reply quote and a link go to their message; older pages load above without moving what you read", async ({ browser }) => {
  const group = await newGroup("Thread long");
  const original = `The original question ${stamp()}`;
  const first = await send("ada", group, { message: original });
  // Four pages after it, so neither the original nor the linked message is loaded when the thread opens.
  let linked: Sent | null = null;
  for (let index = 1; index <= 200; index++) {
    const sent = await send(index % 3 ? "bo" : "ada", group, { message: `Filler ${index} of the long thread, so it needs older pages` });
    if (index === 30) linked = sent;
  }
  const answer = `The answer, at last ${stamp()}`;
  const last = await send("bo", group, { message: answer, reply: { "message-id": first["message-id"] } });
  // Ada has read it all: the thread opens at the bottom (with unread messages it would open at the first of them).
  await dmApi("ada", "POST", `/api/v1/conversations/${encodeURIComponent(group)}/receipts`, { type: "receipts.read", data: { up_to: last["message-id"] } });

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  await expect(row(page, answer)).toBeVisible();
  // The original is three pages back (its words also show in the answer's quote, so it is found by its code).
  const target = page.locator(`article[data-message-code="${first["message-id"]}"]`);
  await expect(target).toHaveCount(0);

  // The quote loads the pages back to the original, scrolls there, selects it and names it in the address.
  await row(page, answer).getByRole("button", { name: /^Reply to Ada/ }).click();
  await expect(target).toBeInViewport({ timeout: 20_000 });
  await expect(target).toHaveAttribute("data-highlight", "");
  await expect(page).toHaveURL(new RegExp(`#${first["message-id"]}$`));

  // A link to a message opens there.
  await page.goto(`/c/${group}#${linked!["message-id"]}`);
  const linkedRow = page.locator(`article[data-message-code="${linked!["message-id"]}"]`);
  await expect(linkedRow).toBeInViewport({ timeout: 20_000 });
  await expect(linkedRow).toHaveAttribute("data-highlight", "");

  // From the bottom, scroll up until older messages arrive: the message read before stays where the wheel left it.
  await page.goto(`/c/${group}`);
  await expect(row(page, answer)).toBeVisible();
  const box = await scroller(page).boundingBox();
  await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
  let checked = false;
  for (let step = 0; step < 60 && !checked; step++) {
    const before = await page.evaluate(() => {
      const log = document.querySelector("[role='log']")!;
      const area = log.closest("[class*='scroller']")!.getBoundingClientRect();
      const hit = document.elementFromPoint(area.left + area.width / 2, area.top + 60)?.closest<HTMLElement>("article[data-message-code]");
      return { count: log.querySelectorAll("article[data-message-code]").length, code: hit?.dataset.messageCode ?? null, top: hit?.getBoundingClientRect().top ?? 0 };
    });
    await page.mouse.wheel(0, -500);
    await page.waitForTimeout(250);
    const after = await page.evaluate(code => {
      const log = document.querySelector("[role='log']")!;
      const element = code ? log.querySelector<HTMLElement>(`article[data-message-code="${code}"]`) : null;
      return { count: log.querySelectorAll("article[data-message-code]").length, top: element?.getBoundingClientRect().top ?? null };
    }, before.code);
    if (before.code && after.top !== null && after.count > before.count) {
      // Older messages came in above during this step: the message moved by the wheel's 500 px (or less at the top), not by the new page's height.
      expect(after.top - before.top).toBeGreaterThan(-2);
      expect(after.top - before.top).toBeLessThan(520);
      checked = true;
    }
  }
  expect(checked, "an older page loaded while scrolling up").toBe(true);

  expect(errors()).toEqual([]);
  await ada.close();
});

/* Delete in place, edit history --------------------------------------------------------------------------------------- */

test("deleting asks in place, and an edit keeps its history", async ({ browser }) => {
  const group = await newGroup("Thread edits");
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const field = page.getByRole("textbox", { name: /^Message / });

  const draft = `Draft wording ${stamp()}`;
  await field.fill(draft);
  await field.press("Enter");
  await expect(row(page, draft)).toHaveAttribute("data-message-code", /.+/);
  // ↑ in the empty field edits it; the row then says Edited, and Edited shows what it said before.
  await field.press("ArrowUp");
  const final = `Final wording ${stamp()}`;
  const editField = page.getByRole("textbox", { name: "Edit your message" });
  await editField.fill(final);
  await editField.press("Enter");
  const edited = row(page, final);
  const code = await edited.getAttribute("data-message-code");
  await edited.getByRole("button", { name: /^Edited/ }).click();
  const history = page.getByRole("dialog").filter({ hasText: "Before the last edit" });
  await expect(history).toContainText(draft);
  await page.keyboard.press("Escape");
  await expect(history).toHaveCount(0);

  // Delete on the selected message asks in place, with the keyboard on the question; Esc keeps it, Delete removes it.
  await field.click();
  await field.press(`${MOD}+ArrowUp`);
  await expect.poll(() => focusedLabel(page)).toContain(final);
  await page.keyboard.press("Delete");
  const question = edited.getByText("Delete for everyone?");
  await expect(question).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(question).toHaveCount(0);
  await expect.poll(() => focusedLabel(page)).toContain(final);
  await page.keyboard.press("Backspace");
  await expect(question).toBeVisible();
  await edited.getByRole("button", { name: "Delete", exact: true }).click();
  const tombstone = page.locator(`article[data-message-code="${code}"]`);
  await expect(tombstone).toHaveAttribute("data-deleted", "");
  await expect(tombstone).toContainText("This message was deleted");

  expect(errors()).toEqual([]);
  await ada.close();
});

/* The keyboard -------------------------------------------------------------------------------------------------------- */

test("the keyboard moves through messages and acts on the selected one, without traps", async ({ browser }) => {
  const group = await newGroup("Thread keys");
  const one = `First from Bo ${stamp()}`;
  const two = `Second from Ada ${stamp()}`;
  const three = `Third from Bo, a link https://example.test/docs ${stamp()}`;
  await send("bo", group, { message: one });
  await send("ada", group, { message: two });
  await send("bo", group, { message: three });

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(row(page, three)).toBeVisible();

  // ⌘↑ from the composer selects the last message; ↑ and K go back, J forward; the selected message is named in full.
  await field.click();
  await field.press(`${MOD}+ArrowUp`);
  await expect.poll(() => focusedLabel(page)).toMatch(/^Message from Bo Okafor at \d\d:\d\d: Third from Bo/);
  await page.keyboard.press("ArrowUp");
  await expect.poll(() => focusedLabel(page)).toMatch(/^Your message at \d\d:\d\d, (Sent|Delivered|Read): Second from Ada/);
  await page.keyboard.press("k");
  await expect.poll(() => focusedLabel(page)).toContain("First from Bo");
  await page.keyboard.press("j");
  await expect.poll(() => focusedLabel(page)).toContain("Second from Ada");

  // Tab walks the selected message's own controls (its toolbar), then leaves the thread for the composer.
  await page.keyboard.press("ArrowDown");
  await expect.poll(() => focusedLabel(page)).toContain("Third from Bo");
  await page.keyboard.press("Tab");
  await expect.poll(() => focusedLabel(page)).toBe("Bo Okafor");
  const visited: string[] = [];
  for (let index = 0; index < 8; index++) {
    await page.keyboard.press("Tab");
    const label = await focusedLabel(page);
    visited.push(label);
    if (/^Message /.test(label)) break;
  }
  expect(visited).toContain("Reply");
  expect(visited.at(-1)).toMatch(/^Message /);
  expect(visited.some(label => label.includes("First from Bo"))).toBe(false);

  // I shows the message's details and its CLI commands; Esc closes them.
  await field.press(`${MOD}+ArrowUp`);
  await page.keyboard.press("i");
  const details = page.getByRole("dialog", { name: /^Message / });
  await expect(details).toContainText("dm messages show");
  await page.keyboard.press("Escape");
  await expect(details).toHaveCount(0);

  // R replies (the composer takes the keyboard); Esc cancels the reply; Esc on a message goes back to the composer.
  await field.press(`${MOD}+ArrowUp`);
  await page.keyboard.press("r");
  await expect(page.getByText(/^Replying to Bo Okafor/)).toBeVisible();
  await expect(field).toBeFocused();
  await field.press("Escape");
  await expect(page.getByText(/^Replying to/)).toHaveCount(0);
  await field.press(`${MOD}+ArrowUp`);
  await page.keyboard.press("Escape");
  await expect(field).toBeFocused();

  // C and L copy the message's words and its link.
  await field.press(`${MOD}+ArrowUp`);
  await page.keyboard.press("c");
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toContain("Third from Bo");
  await page.keyboard.press("l");
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toMatch(new RegExp(`/c/${group}#[0-9a-z]{3,}$`));

  expect(errors()).toEqual([]);
  await ada.close();
});

/* Photos -------------------------------------------------------------------------------------------------------------- */

test("photos open in a lightbox by keyboard: Enter, → to the next, Esc back to the photo", async ({ browser }) => {
  const group = await newGroup("Thread photos");
  const caption = `Two photos ${stamp()}`;
  await send("bo", group, {
    message: caption,
    attachments: [
      { permanent_url: "https://img.example.test/a-harbour.png", name: "a-harbour.png", content_type: "image/png", size: 120_400 },
      { permanent_url: "https://img.example.test/b-hills.png", name: "b-hills.png", content_type: "image/png", size: 98_100 },
    ],
  });
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(row(page, caption)).toBeVisible();

  await field.click();
  await field.press(`${MOD}+ArrowUp`);
  await expect.poll(() => focusedLabel(page)).toContain(caption);
  let found = false;
  for (let index = 0; index < 5 && !found; index++) {
    await page.keyboard.press("Tab");
    found = (await focusedLabel(page)).startsWith("Open photo 1 of 2");
  }
  expect(found, "Tab reaches the first photo").toBe(true);
  await page.keyboard.press("Enter");
  const lightbox = page.getByRole("dialog", { name: "a-harbour.png" });
  await expect(lightbox).toBeVisible();
  await expect(lightbox).toContainText("Photo 1 of 2");
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("dialog", { name: "b-hills.png" })).toContainText("Photo 2 of 2");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect.poll(() => focusedLabel(page)).toMatch(/^Open photo 1 of 2/);

  expect(errors()).toEqual([]);
  await ada.close();
});

/* The menu, keys that do not apply, code blocks, links that cannot be followed --------------------------------------- */

test("the menu gives the keyboard back; keys that do not apply do nothing; code is one Tab away; a bad link is answered at once", async ({ browser }) => {
  const group = await newGroup("Thread menu");
  const theirs = `Words from Bo ${stamp()}`;
  const code = `Code from Bo ${stamp()}\n\n\`\`\`ts\nexport const line = "${"a long line of code that cannot fit in the bubble and scrolls inside its own block ".repeat(3)}";\n\`\`\``;
  const mine = `Words from Ada ${stamp()}`;
  await send("bo", group, { message: theirs });
  await send("bo", group, { message: code });
  await send("ada", group, { message: mine });

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(row(page, mine)).toHaveAttribute("data-message-code", /.+/);

  // Shift+F10 opens the selected message's menu; Esc closes it and the message has the keyboard again.
  await field.click();
  await field.press(`${MOD}+ArrowUp`);
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  await expect.poll(() => focusedLabel(page)).toContain(theirs);
  await page.keyboard.press("Shift+F10");
  await expect(page.getByRole("menu", { name: "Message actions" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect.poll(() => focusedLabel(page)).toContain(theirs);

  // E (someone else's message) and Space (a Carbon's words cannot be listened to) do nothing: nothing is typed.
  await page.keyboard.press("e");
  await page.keyboard.press(" ");
  await expect(field).toHaveValue("");
  await expect.poll(() => focusedLabel(page)).toContain(theirs);

  // The thread is one Tab stop: from the log, Tab leaves the thread (it does not stop at the code block on the way).
  await page.getByRole("log").focus();
  await page.keyboard.press("Tab");
  expect(await page.evaluate(() => !!document.activeElement?.closest("[role='log']"))).toBe(false);
  // The selected message's own controls include its code block, which scrolls with the keyboard.
  await field.press(`${MOD}+ArrowUp`);
  await page.keyboard.press("ArrowUp");
  await expect.poll(() => focusedLabel(page)).toContain("Code from Bo");
  let reached = false;
  for (let index = 0; index < 4 && !reached; index++) {
    await page.keyboard.press("Tab");
    reached = (await focusedLabel(page)) === "ts block";
  }
  expect(reached, "Tab reaches the selected message's code block").toBe(true);

  // A link to a message no older page can bring is answered at once, without loading the conversation's history.
  let pages = 0;
  page.on("request", request => { if (request.method() === "GET" && /\/messages(\?|$)/.test(request.url())) pages++; });
  await page.goto(`/c/${group}#zzz`);
  await expect(page.getByText("Message zzz is not in this thread yet")).toBeVisible();
  expect(pages).toBeLessThanOrEqual(1);

  expect(errors()).toEqual([]);
  await ada.close();
});

/* One toolbar, never over another message ---------------------------------------------------------------------------- */

test("one message's toolbar at a time, never over another: beside it, or under the message you select on a phone", async ({ browser }) => {
  const group = await newGroup("Thread toolbars");
  const long = (who: string) => `${who} ${stamp()}: long enough to fill most of the column on a phone, so nothing fits beside it and it wraps onto lines.`;
  const first = long("First from Bo");
  const second = long("Then Bo again");
  const answer = long("Ada answers");
  const last = long("Last from Bo");
  await send("bo", group, { message: first });
  await send("bo", group, { message: second });
  await send("ada", group, { message: answer });
  await send("bo", group, { message: "ok" });
  await send("bo", group, { message: last });

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(row(page, last)).toBeVisible();

  // Wide: the pointer rests on a message (its toolbar beside it); the keyboard selects another: only that one shows,
  // and the pointer resting elsewhere does not add a second.
  await row(page, second).locator("[data-sq='surface']").first().hover();
  await expect.poll(async () => (await toolbars(page)).map(bar => bar.row)).toEqual([expect.stringContaining("Then Bo again")]);
  await field.click();
  await field.press(`${MOD}+ArrowUp`);
  await expect.poll(() => focusedLabel(page)).toContain("Last from Bo");
  await expect.poll(async () => (await toolbars(page)).map(bar => bar.row)).toEqual([expect.stringContaining("Last from Bo")]);
  await row(page, first).locator("[data-sq='surface']").first().hover();
  await page.waitForTimeout(200);
  expect((await toolbars(page)).map(bar => bar.row)).toEqual([expect.stringContaining("Last from Bo")]);
  for (const bar of await toolbars(page)) expect(bar.covered, `${bar.row} covers nothing`).toBe(0);

  // A phone: tapping a message selects it; its toolbar opens under it (or beside a short one), covering nothing.
  await page.setViewportSize({ width: 390, height: 844 });
  for (const words of [first, answer, "ok", last]) {
    const target = words === "ok" ? page.locator("article[data-message-row][aria-label$=': ok']") : row(page, words);
    await target.locator("[data-sq='surface']").first().click({ position: { x: 16, y: 10 } });
    await expect.poll(async () => (await toolbars(page)).length).toBe(1);
    const [bar] = await toolbars(page);
    expect(bar!.row).toContain(words === "ok" ? ": ok" : words.slice(0, 12));
    expect(["side", "below"]).toContain(bar!.place);
    expect(bar!.covered, `the toolbar of "${bar!.row}" covers nothing`).toBe(0);
    if (words !== "ok") expect(bar!.place).toBe("below");
  }
  // Pressing outside the selected message lets it go.
  await page.getByRole("heading", { level: 1 }).click();
  await expect.poll(async () => (await toolbars(page)).length).toBe(0);

  expect(errors()).toEqual([]);
  await ada.close();
});

/* A conversation of several, and drafts on opening ----------------------------------------------------------------- */

test("a conversation of several says how many people it has; opening it asks for a draft at most once", async ({ browser }) => {
  const several = await dmApi<{ id: string }>("ada", "POST", "/api/v1/conversations", { type: "create_conversation", data: { participant_ids: ["c:dmc-bo", "c:dmc-cy"] } });
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  const drafts: number[] = [];
  page.on("response", response => {
    if (response.request().method() === "GET" && /\/draft(\?|$)/.test(response.url())) drafts.push(response.status());
  });
  await page.goto(`/c/${several.id}`);
  const header = page.locator("header").filter({ has: page.getByRole("heading", { level: 1 }) });
  await expect(header).toContainText("3 people");
  await expect(header).not.toContainText("Group");
  await expect(page.getByText(/^This is the beginning of your conversation with (Bo Okafor and Cy Varga|Cy Varga and Bo Okafor)\./)).toBeVisible();
  await page.waitForTimeout(1500);
  expect(drafts.length, `GET …/draft on opening: ${drafts.join(", ")}`).toBeLessThanOrEqual(1);
  expect(drafts.every(status => status === 200 || status === 404), "no draft is an answer (404), never a failure").toBe(true);

  expect(errors()).toEqual([]);
  await ada.close();
});

/* A custodian's view ---------------------------------------------------------------------------------------------- */

test("a custodian's view offers only what a custodian may do: read, and listen for the Silicon", async ({ browser }) => {
  // Bo and Scout talk (Scout allows Bo); Ada looks after Scout.
  const conversation = await dmApi<{ id: string }>("bo", "POST", "/api/v1/conversations", { type: "create_conversation", data: { participant_ids: ["si:dmc-scout"] } });
  const words = `From Scout to Bo ${stamp()}`;
  const sent = await send("scout", conversation.id, { message: words });

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${conversation.id}?for=si:dmc-scout`);
  await expect(page.getByText(/^You are reading Scout.s conversation as its custodian/)).toBeVisible();
  const message = page.locator(`article[data-message-code="${sent["message-id"]}"]`);
  await expect(message).toBeVisible();

  // I: the message's commands are the ones that work in a custodian's view (no reply; listening names the Silicon).
  await message.focus();
  await page.keyboard.press("i");
  const details = page.getByRole("dialog", { name: `Message ${sent["message-id"]}` });
  await expect(details).toContainText(`dm messages show ${conversation.id} ${sent["message-id"]}`);
  await expect(details).toContainText("--for si:dmc-scout");
  await expect(details).not.toContainText("--reply-to");
  await page.keyboard.press("Escape");

  // The header's CLI command reads the conversation; it never sends as the Silicon.
  await page.getByRole("button", { name: "More for this conversation" }).click();
  await page.getByRole("menuitem", { name: "Copy the CLI command" }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(`dm messages list ${conversation.id}`);

  expect(errors()).toEqual([]);
  await ada.close();
});
