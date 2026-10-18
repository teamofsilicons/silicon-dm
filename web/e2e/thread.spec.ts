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
 *   one action layer at a time: a menu, the Edited popover or an account card puts the toolbar away (a phone's ⋯ too)
 *   photos keep their shape: one alone, two side by side sharing a height
 *   Message details in names (who, when, where) with the ids under them; nothing cut on a phone
 *   offline, nobody is said to be online (the thread's standing region says it is offline; a phone's banner is short)
 *   a message routed to an intent says so in a group; a custodian deletes the viewed Silicon's own messages, nothing else
 *   switching conversations (a row, the open one's row, the palette, ⌥↓, a fresh load) gives the message field the
 *     keyboard; on a phone's layout Back gives it to the row
 *   keyboard focus on the thread's own controls changes them by 3:1 or more (WCAG 1.4.11), measured pixel by pixel
 *   a photo that could not load comes back (by itself, or with Try again); a file under a photo is as wide as it
 *   a conversation that is not here, or a group you are only invited to, is said in a Carbon's words
 *   on a phone, a tapped message stays under the finger
 *   the first message of a conversation that opened empty is read out by the log
 */
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { expect, test, type Browser, type BrowserContext, type Locator, type Page } from "@playwright/test";
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

/** A photo for made-up https addresses, served by the test (no internet): 800 × 600, or the size its name says (…-1600x900-…). */
const photo = (label: string, fill: string) => {
  const [, width = "800", height = "600"] = /(\d+)x(\d+)/.exec(label) ?? [];
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}"><rect width="100%" height="100%" fill="${fill}"/><text x="40" y="${Number(height) - 40}" font-family="sans-serif" font-size="48">${label}</text></svg>`;
};

async function signedIn(browser: Browser, who: string, options: Parameters<Browser["newContext"]>[0] = {}): Promise<BrowserContext> {
  const cookie = JSON.parse(helper("cookie", who)) as { name: string; value: string; httpOnly?: boolean; sameSite?: "Lax" | "Strict" | "None" };
  const context = await browser.newContext({ baseURL: BASE, permissions: ["clipboard-read", "clipboard-write"], ...options });
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
      .filter(node => !node.hidden && node.getBoundingClientRect().width > 0 && getComputedStyle(node).visibility !== "hidden")
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
const escaped = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * How much keyboard focus changes a control, as WCAG 1.4.11 measures a focus indicator: each pixel of the control (and
 * 4 px around it) with the keyboard on it, against the same pixel at rest (transitions off), as a contrast ratio.
 * `strong` counts the pixels that change by 3:1 or more (a 2 px ring all around is about twice the control's perimeter
 * of them). `measure` is where to look when that is not the control itself (a message's bubble); `select` puts the
 * keyboard there another way than focus() (then the rest is what it was before).
 */
async function focusChange(page: Page, target: Locator, options: { measure?: Locator; select?: () => Promise<void> } = {}): Promise<{ strong: number; perimeter: number; visible: boolean }> {
  const measure = options.measure ?? target;
  await page.addStyleTag({ content: "*, *::before, *::after { transition: none !important; animation: none !important; }" });
  await target.scrollIntoViewIfNeeded();
  await page.mouse.move(1, 1);
  const box = (await measure.boundingBox())!;
  const viewport = page.viewportSize()!;
  const x = Math.max(0, box.x - 4);
  const y = Math.max(0, box.y - 4);
  const clip = { x, y, width: Math.min(viewport.width - x, box.width + 8), height: Math.min(viewport.height - y, box.height + 8) };
  if (!options.select) await page.evaluate(() => (document.activeElement instanceof HTMLElement ? document.activeElement.blur() : undefined));
  const rest = await page.screenshot({ clip });
  if (options.select) await options.select();
  else {
    // A key first, so the browser shows focus as it does for the keyboard (:focus-visible).
    await page.keyboard.press("Shift");
    await target.focus();
  }
  const visible = await target.evaluate(element => element.matches(":focus-visible"));
  const focused = await page.screenshot({ clip });
  const strong = await page.evaluate(async ([a, b]) => {
    const pixels = async (base64: string) => {
      const bitmap = await createImageBitmap(new Blob([Uint8Array.from(atob(base64), c => c.charCodeAt(0))], { type: "image/png" }));
      const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
      const context = canvas.getContext("2d")!;
      context.drawImage(bitmap, 0, 0);
      return context.getImageData(0, 0, bitmap.width, bitmap.height).data;
    };
    const [p, q] = await Promise.all([pixels(a), pixels(b)]);
    const channel = (value: number) => {
      const c = value / 255;
      return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
    };
    const luminance = (d: Uint8ClampedArray, i: number) => 0.2126 * channel(d[i]!) + 0.7152 * channel(d[i + 1]!) + 0.0722 * channel(d[i + 2]!);
    let count = 0;
    for (let i = 0; i < p.length; i += 4) {
      const one = luminance(p, i);
      const two = luminance(q, i);
      if ((Math.max(one, two) + 0.05) / (Math.min(one, two) + 0.05) >= 3) count++;
    }
    return count;
  }, [rest.toString("base64"), focused.toString("base64")] as const);
  return { strong, perimeter: Math.round(2 * (box.width + box.height)), visible };
}

/** Keyboard focus on a control changes at least three quarters of its edge's length of pixels by 3:1 or more. */
async function expectFocusSeen(page: Page, name: string, target: Locator, options?: Parameters<typeof focusChange>[2]): Promise<void> {
  const change = await focusChange(page, target, options);
  expect(change.visible, `${name}: the keyboard is on it (:focus-visible)`).toBe(true);
  expect(change.strong, `${name}: pixels that change by 3:1 or more with keyboard focus (WCAG 1.4.11), against an edge of ${change.perimeter} px`).toBeGreaterThanOrEqual(change.perimeter * 0.75);
}
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

  // Bo types: Ada's header and the end of her thread say so (presence is read every 15 s), where the DM service says
  // what someone does per conversation (features.presence_activity); a service that does not is never taken at its
  // word for this conversation (lib/chat/presence.ts; e2e/foundation.spec.ts checks that case).
  const contracts = (await (await fetch(`${DM_API}/api/v1/contracts`)).json()) as { data?: { features?: { presence_activity?: { scope?: string } } } };
  if (contracts.data?.features?.presence_activity?.scope === "conversation") {
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
  } else {
    test.info().annotations.push({ type: "skipped part", description: "typing: this DM service does not say what someone does per conversation" });
  }

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
  // Every row back to it is drawn now; back at the latest, only the newest stay (they come back from the cache above).
  expect(await page.locator("article[data-message-row]").count()).toBeGreaterThan(190);
  await page.getByRole("button", { name: /^Jump to latest/ }).click();
  await expect(row(page, answer)).toBeInViewport();
  await expect.poll(() => page.locator("article[data-message-row]").count(), { message: "the rows far back go once the thread is at its latest" }).toBeLessThanOrEqual(140);

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
  // The history is named, and its first item is the words as they are now, so both read together wherever it opens.
  const history = page.getByRole("dialog", { name: "Versions of this message" });
  await expect(history).toContainText(draft);
  await expect(history.getByRole("listitem").first()).toContainText(`Now${final}`);
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
  // The keyboard stays where it is: the dialog says which photo it shows now.
  await expect(page.getByRole("dialog").getByRole("status")).toHaveText("Photo 2 of 2, b-hills.png");
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
  // It comes and goes inside the log, which reads out only new messages: the toolbar is not one.
  await expect(page.locator("[role='toolbar']")).toHaveAttribute("aria-live", "off");
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

test("a custodian's view offers only what a custodian may do: read, listen for the Silicon (and delete its messages)", async ({ browser }) => {
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

/* One action layer at a time --------------------------------------------------------------------------------------- */

test("one action layer at a time: a menu, the Edited popover or an account card puts the toolbar away", async ({ browser }) => {
  const group = await newGroup("Thread layers");
  const words = `Layers ${stamp()}`;
  const first = await send("bo", group, { message: `${words} draft` });
  await dmApi("bo", "PATCH", `/api/v1/conversations/${encodeURIComponent(group)}/messages/${first["message-id"]}`, { type: "message.updated", data: { message: `${words} final` } });
  const after = `After ${stamp()}`;
  await send("bo", group, { message: after });

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const edited = page.locator(`article[data-message-code="${first["message-id"]}"]`);
  await expect(edited).toBeVisible();
  const shown = async () => (await toolbars(page)).map(bar => bar.row);

  // The pointer on the edited message: its toolbar. Edited opens its history: the toolbar steps back meanwhile.
  await edited.locator("[data-sq='surface']").first().hover();
  await expect.poll(shown).toEqual([expect.stringContaining(`${words} final`)]);
  await edited.getByRole("button", { name: /^Edited/ }).click();
  await expect(page.getByRole("dialog").filter({ hasText: "Before the last edit" })).toBeVisible();
  await expect.poll(shown).toEqual([]);
  await page.keyboard.press("Escape");
  await expect.poll(shown).toHaveLength(1);

  // The menu (right click) is the one layer: no toolbar shows while it is open.
  await row(page, after).locator("[data-sq='surface']").first().click({ button: "right" });
  await expect(page.getByRole("menu", { name: "Message actions" })).toBeVisible();
  await expect.poll(shown).toEqual([]);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu", { name: "Message actions" })).toHaveCount(0);

  // An account card (Bo's name) is one too.
  await page.mouse.move(5, 5);
  await edited.getByRole("button", { name: "Bo Okafor", exact: true }).hover();
  await expect(page.getByRole("tooltip")).toContainText("c:dmc-bo");
  await expect.poll(shown).toEqual([]);
  await page.mouse.move(5, 5);
  await expect(page.getByRole("tooltip")).toHaveCount(0);

  // A phone: a tapped message's ⋯ opens its menu (the selection holds), and the toolbar keeps its place, unseen.
  await page.setViewportSize({ width: 390, height: 844 });
  await edited.locator("[data-sq='surface']").first().click({ position: { x: 16, y: 10 } });
  await expect.poll(shown).toEqual([expect.stringContaining(`${words} final`)]);
  await edited.getByRole("button", { name: "More actions" }).click();
  await expect(page.getByRole("menu", { name: "Message actions" })).toBeVisible();
  await expect.poll(shown).toEqual([]);
  await expect(edited.locator("[role='toolbar']")).toHaveAttribute("data-held", "");
  await page.keyboard.press("Escape");
  await expect.poll(shown).toEqual([expect.stringContaining(`${words} final`)]);

  expect(errors()).toEqual([]);
  await ada.close();
});

/* Photos keep their shape -------------------------------------------------------------------------------------------- */

test("photos keep their shape: one alone at its own, two side by side sharing a height", async ({ browser }) => {
  const group = await newGroup("Thread shapes");
  const alone = `One wide ${stamp()}`;
  const pair = `A pair ${stamp()}`;
  await send("bo", group, { message: alone, attachments: ["https://img.example.test/shape-1600x900-wide.svg"] });
  await send("bo", group, { message: pair, attachments: ["https://img.example.test/shape-1600x900-left.svg", "https://img.example.test/shape-900x1200-right.svg"] });

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const boxes = async (text: string) =>
    row(page, text).locator("button[aria-label^='Open']").evaluateAll(buttons => buttons.map(button => {
      const rect = button.getBoundingClientRect();
      return { width: rect.width, height: rect.height };
    }));
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 900 });
    await expect.poll(async () => {
      const [photo] = await boxes(alone);
      return photo ? Math.round((photo.width / photo.height) * 100) / 100 : 0;
    }, { message: `at ${width}: one photo at its own shape (16:9)` }).toBeCloseTo(16 / 9, 1);
    await expect.poll(async () => {
      const [left, right] = await boxes(pair);
      if (!left || !right) return "not drawn";
      const same = Math.abs(left.height - right.height) <= 1;
      const shapes = [left.width / left.height, right.width / right.height];
      return same && Math.abs(shapes[0]! - 16 / 9) < 0.08 && Math.abs(shapes[1]! - 0.75) < 0.05 ? "kept" : JSON.stringify({ left, right });
    }, { message: `at ${width}: two side by side, one height, each its own shape` }).toBe("kept");
  }
  expect(errors()).toEqual([]);
  await ada.close();
});

/* Message details in names ------------------------------------------------------------------------------------------ */

test("Message details say who, when and where in names, with the ids under them; nothing is cut on a phone", async ({ browser }) => {
  const group = await newGroup("Thread details");
  const name = (await dmApi<{ group: { name: string } }>("ada", "GET", `/api/v1/groups/${encodeURIComponent(group)}`)).group.name;
  const words = `Details ${stamp()}`;
  const sent = await send("bo", group, { message: words, attachments: ["https://img.example.test/a-photo.svg"] });

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const message = page.locator(`article[data-message-code="${sent["message-id"]}"]`);
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 900 });
    await message.focus();
    await page.keyboard.press("i");
    const details = page.getByRole("dialog", { name: `Message ${sent["message-id"]}` });
    await expect(details).toContainText(new RegExp(`Sent today at \\d\\d:\\d\\d by Bo Okafor in ${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\.`));
    await expect(details).toContainText(`c:dmc-bo in ${group}`);
    // Every key and value whole (a long line scrolls inside the viewer); on a phone the viewer grows, so the dialog
    // alone scrolls up and down.
    const cut = await details.locator("[role='tree']").evaluate(tree => [...tree.querySelectorAll<HTMLElement>("[class*='__key'], [class*='__value']")].filter(node => node.scrollWidth > node.clientWidth + 1).map(node => node.textContent));
    if (width === 390) {
      expect(cut, "nothing cut on a phone").toEqual([]);
      expect(await details.locator("[role='tree']").evaluate(tree => tree.scrollHeight <= tree.clientHeight + 1), "no scroll inside the dialog's scroll").toBe(true);
    }
    await page.keyboard.press("Escape");
    await expect(details).toHaveCount(0);
  }
  expect(errors()).toEqual([]);
  await ada.close();
});

/* Offline ------------------------------------------------------------------------------------------------------------- */

test("offline, the header stops saying who is online and says it waits for the network", async ({ browser }) => {
  const direct = await dmApi<{ id: string }>("ada", "POST", "/api/v1/conversations", { type: "create_conversation", data: { participant_ids: ["c:dmc-bo"] } });
  const device = `e2e-offline-${stamp()}`;
  await dmApi("bo", "PUT", `/api/v1/presence/devices/${device}`, { type: "renew_presence", data: { activity: null } });
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  try {
    await page.goto(`/c/${direct.id}`);
    const status = page.locator("header").filter({ has: page.getByRole("heading", { level: 1 }) }).locator("p").first();
    await expect(status).toContainText("online", { timeout: 20_000 });
    // The thread's region for such news is there before anything happens (one added with its words is often not
    // read), and says it.
    const said = page.getByRole("main").getByRole("status").filter({ hasText: "You are offline." });
    await expect(said).toHaveCount(0);
    await ada.setOffline(true);
    await expect(status).toHaveText("waiting for the network");
    await expect(said).toHaveCount(1);
    // On a phone: a short sentence, its icon on its first line, read from the start.
    await page.setViewportSize({ width: 390, height: 844 });
    const banner = page.locator("p[data-tone='offline']");
    await expect(banner).toHaveText("Offline: what you send goes out when you are back.", { useInnerText: true });
    const fits = await banner.evaluate(node => {
      const box = node.getBoundingClientRect();
      const icon = node.querySelector("svg")!.getBoundingClientRect();
      return { lines: Math.round((box.height - 16) / 20), iconFirstLine: icon.top - box.top < 20, left: getComputedStyle(node).textAlign };
    });
    expect(fits.lines, "two lines at most").toBeLessThanOrEqual(2);
    expect(fits.iconFirstLine).toBe(true);
    expect(fits.left).toBe("left");
    await page.setViewportSize({ width: 1280, height: 800 });
    await ada.setOffline(false);
    await expect(status).toContainText("online", { timeout: 20_000 });
    await expect(status).not.toContainText("waiting");
  } finally {
    await dmApi("bo", "DELETE", `/api/v1/presence/devices/${device}`).catch(() => undefined);
  }
  // Offline, the page's own requests fail on purpose; what a Carbon would mind is checked above.
  expect(errors().filter(error => !/Failed to fetch|ERR_INTERNET_DISCONNECTED|network/i.test(error))).toEqual([]);
  await ada.close();
});

/* Routing and a custodian's delete ----------------------------------------------------------------------------------- */

test("a message routed to an intent says so in a group; a custodian deletes the viewed Silicon's own messages only", async ({ browser }) => {
  // Bo's group with Ada, her Silicon Scout and his Silicon Atlas; Scout routes one message to Atlas's notes intent.
  const created = await dmApi<{ id: string }>("bo", "POST", "/api/v1/groups", { type: "create_group", data: { name: `Thread custody ${stamp()}`, member_ids: ["c:dmc-ada", "si:dmc-scout", "si:dmc-atlas"] } });
  const group = created.id;
  for (const who of ["ada", "scout", "atlas"]) await dmApi(who, "POST", `/api/v1/groups/${encodeURIComponent(group)}/accept`).catch(() => undefined);
  const mark = stamp();
  const one = await send("scout", group, { message: `Scout one ${mark}` });
  await send("scout", group, { message: `Scout routed ${mark}`, recipient_id: "notes@si:dmc-atlas" });
  const three = await send("scout", group, { message: `Scout three ${mark}` });
  await send("atlas", group, { message: `Atlas here ${mark}` });

  // Bo reads the routing in the group.
  const bo = await signedIn(browser, "bo");
  const friend = await bo.newPage();
  const friendErrors = threadErrors(friend);
  await friend.goto(`/c/${group}`);
  await expect(row(friend, `Scout routed ${mark}`)).toContainText("→ notes");
  await expect(row(friend, `Scout routed ${mark}`)).toContainText("routed to the notes intent of si:dmc-atlas");
  expect(friendErrors()).toEqual([]);
  await bo.close();

  // Ada, viewing as Scout (she is in the group herself too): Delete on Scout's messages, never on another's.
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}?for=si:dmc-scout`);
  await expect(page.getByText(/You can delete Scout.s own messages/)).toBeVisible();
  const menuOn = async (text: string) => {
    await row(page, text).locator("[data-sq='surface']").first().click({ button: "right" });
    const items = (await page.getByRole("menuitem").allInnerTexts()).map(item => item.split("\n")[0]);
    await page.keyboard.press("Escape");
    return items;
  };
  expect(await menuOn(`Atlas here ${mark}`)).not.toContain("Delete…");
  expect(await menuOn(`Scout one ${mark}`)).toContain("Delete…");
  await row(page, `Scout one ${mark}`).locator("[data-sq='surface']").first().click({ button: "right" });
  await page.getByRole("menuitem", { name: /^Delete/ }).click();
  await expect(row(page, `Scout one ${mark}`).getByText("Delete for everyone?")).toBeVisible();
  // The question has the keyboard on Delete: Enter deletes.
  await page.keyboard.press("Enter");
  await expect(page.locator(`article[data-message-code="${one["message-id"]}"]`)).toHaveAttribute("data-deleted", "");
  // The Delete key on a selected message of Scout's asks the same; on Atlas's it does nothing.
  await page.locator(`article[data-message-code="${three["message-id"]}"]`).focus();
  await page.keyboard.press("Delete");
  await page.keyboard.press("Enter");
  await expect(page.locator(`article[data-message-code="${three["message-id"]}"]`)).toHaveAttribute("data-deleted", "");
  await row(page, `Atlas here ${mark}`).focus();
  await page.keyboard.press("Delete");
  await page.waitForTimeout(300);
  await expect(page.getByText("Delete for everyone?")).toHaveCount(0);
  const tombstones = (await dmApi<{ items: Array<{ "message-id": string; deleted_at: string | null }> }>("bo", "GET", `/api/v1/conversations/${encodeURIComponent(group)}/messages?limit=10`)).items.filter(item => item.deleted_at).map(item => item["message-id"]);
  expect(tombstones.sort()).toEqual([one["message-id"], three["message-id"]].sort());
  expect(errors()).toEqual([]);
  await ada.close();
});

/* Switching conversations, then typing ---------------------------------------------------------------------------- */

test("switching conversations gives the message field the keyboard; on a phone's layout Back gives it to the row", async ({ browser }) => {
  const one = await newGroup("Switch one");
  const two = await newGroup("Switch two");
  const [nameOne, nameTwo] = await Promise.all([one, two].map(async id => (await dmApi<{ group: { name: string } }>("ada", "GET", `/api/v1/groups/${encodeURIComponent(id)}`)).group.name));
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  const field = (name: string) => page.getByRole("textbox", { name: `Message ${name}` });
  const listRow = (address: string) => page.locator(`a[data-row][data-conversation="${address}"]:visible`);

  // Opening a conversation (a fresh load) gives its field the keyboard: the next key is the first word.
  await page.goto(`/c/${one}`);
  await expect(field(nameOne!)).toBeFocused();
  // A row of the list.
  await listRow(two).click();
  await expect(field(nameTwo!)).toBeFocused();
  await page.keyboard.type("hi");
  await expect(field(nameTwo!)).toHaveValue("hi");
  await field(nameTwo!).fill("");
  // The open conversation's own row.
  await page.getByRole("heading", { level: 1 }).click();
  await listRow(two).click();
  await expect(field(nameTwo!)).toBeFocused();
  // A conversation chosen in the palette.
  await page.keyboard.press(`${MOD}+k`);
  await page.keyboard.type(nameOne!);
  await page.getByRole("option", { name: new RegExp(`^${escaped(nameOne!)}`) }).first().click();
  await expect(field(nameOne!)).toBeFocused();
  // ⌥↓ from the field: whichever conversation comes next in the list, its field has the keyboard (or it has none).
  await page.keyboard.press("Alt+ArrowDown");
  await expect(page).not.toHaveURL(new RegExp(`/c/${escaped(one)}$`));
  await expect.poll(() => page.evaluate(() => {
    const next = document.querySelector<HTMLTextAreaElement>("main textarea[data-composer-field]");
    return !!next && (next.disabled || document.activeElement === next);
  }), { message: "the next conversation's field has the keyboard" }).toBe(true);
  // The details toggle keeps its name; whether it is pressed says whether the details show.
  await page.goto(`/c/${one}`);
  const details = page.getByRole("button", { name: "Show details" });
  await expect(details).toHaveAttribute("aria-pressed", /true|false/);
  const before = await details.getAttribute("aria-pressed");
  await details.click();
  await expect(page.getByRole("button", { name: "Show details" })).toHaveAttribute("aria-pressed", before === "true" ? "false" : "true");
  await page.getByRole("button", { name: "Show details" }).click();
  await expect(page.getByRole("button", { name: "Show details" })).toHaveAttribute("aria-pressed", before!);

  // A phone's layout (a narrow window, with a keyboard): a row opens its conversation with the field ready, and Back
  // puts the keyboard on that row again.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/c");
  await listRow(one).focus();
  await page.keyboard.press("Enter");
  await expect(field(nameOne!)).toBeFocused();
  await page.getByRole("link", { name: "Back to conversations" }).click();
  await expect(listRow(one)).toBeFocused();

  expect(errors()).toEqual([]);
  await ada.close();
});

/* Keyboard focus is seen ---------------------------------------------------------------------------------------- */

test("keyboard focus on the thread's own controls changes them by 3:1 or more (WCAG 1.4.11)", async ({ browser }) => {
  const group = await newGroup("Thread focus");
  const words = `Focus ${stamp()}`;
  await send("bo", group, {
    message: words,
    attachments: [
      { permanent_url: "https://img.example.test/a-harbour.png", name: "a-harbour.png", content_type: "image/png", size: 120_400 },
      { permanent_url: "https://img.example.test/b-hills.png", name: "b-hills.png", content_type: "image/png", size: 98_100 },
      { permanent_url: "https://files.example.test/pricing-v3.pdf", name: "pricing-v3.pdf", content_type: "application/pdf", size: 52_000 },
    ],
  });
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  const main = page.getByRole("main");
  const field = page.getByRole("textbox", { name: /^Message / });
  const message = row(page, words);
  await expect(message).toBeVisible();

  await expectFocusSeen(page, "the header's More", main.getByRole("button", { name: "More for this conversation" }));
  await expectFocusSeen(page, "a file's Download", message.getByRole("link", { name: "Download pricing-v3.pdf" }));
  // The selected message (⌘↑ from the field): its bubble's edge.
  await field.focus();
  await expectFocusSeen(page, "the selected message", message, { measure: message.locator("[data-sq='surface']").first(), select: async () => { await field.focus(); await field.press(`${MOD}+ArrowUp`); } });
  // A send the service refused (this page's request only): Retry and Discard.
  const refusing = (url: URL) => url.pathname.endsWith(`/conversations/${encodeURIComponent(group)}/messages`);
  await page.route(refusing, route => (route.request().method() === "POST"
    ? route.fulfill({ status: 422, contentType: "application/json", body: JSON.stringify({ type: "error", data: { error: { code: "validation_error", message: "This test refused the send.", hint: "Nothing to do." } } }) })
    : route.continue()));
  await field.fill(`Refused ${stamp()}`);
  await field.press("Enter");
  const retry = main.getByRole("button", { name: "Retry" });
  await expect(retry).toBeVisible();
  await expectFocusSeen(page, "a failed send's Retry", retry);
  await expectFocusSeen(page, "a failed send's Discard", main.getByRole("button", { name: "Discard" }));
  await main.getByRole("button", { name: "Discard" }).click();
  await page.unroute(refusing);
  // The lightbox's steps.
  await message.getByRole("button", { name: /^Open photo 1 of 2/ }).click();
  const lightbox = page.getByRole("dialog", { name: "a-harbour.png" });
  await expect(lightbox).toBeVisible();
  // Once it has settled (it opens with a short animation).
  await page.waitForTimeout(500);
  await expectFocusSeen(page, "the lightbox's Previous", lightbox.getByRole("button", { name: "Previous photo" }));
  await expectFocusSeen(page, "the lightbox's Next", lightbox.getByRole("button", { name: "Next photo" }));
  await page.keyboard.press("Escape");
  // A phone's Back.
  await page.setViewportSize({ width: 390, height: 844 });
  await expectFocusSeen(page, "Back to conversations", page.getByRole("link", { name: "Back to conversations" }));

  expect(errors()).toEqual([]);
  await ada.close();
});

/* What could not load comes back -------------------------------------------------------------------------------- */

test("a photo that could not load comes back, by itself or with Try again; a file under a photo is as wide as it", async ({ browser }) => {
  const group = await newGroup("Thread retries");
  const words = `Retries ${stamp()}`;
  const tag = stamp();
  await send("bo", group, {
    message: `${words} one`,
    attachments: [
      { permanent_url: `https://img.example.test/a-once-${tag}-1600x900.png`, name: "once.png", content_type: "image/png", size: 80_000 },
      { permanent_url: "https://files.example.test/pricing-v3.pdf", name: "pricing-v3.pdf", content_type: "application/pdf", size: 52_000 },
    ],
  });
  await send("bo", group, { message: `${words} two`, attachments: [{ permanent_url: `https://img.example.test/b-twice-${tag}.png`, name: "twice.png", content_type: "image/png", size: 80_000 }] });
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  // The first photo fails to load once, the second twice (its own retry as well); then both load.
  let once = 1;
  let twice = 2;
  await page.route(new RegExp(`^https://img\\.example\\.test/.*-${tag}`), route => {
    const url = route.request().url();
    if (url.includes("-once-") && once > 0) {
      once--;
      return route.abort("failed");
    }
    if (url.includes("-twice-") && twice > 0) {
      twice--;
      return route.abort("failed");
    }
    return route.fallback();
  });
  await page.goto(`/c/${group}`);
  const first = row(page, `${words} one`);
  const second = row(page, `${words} two`);
  // A card says what happened, with Try again; a few seconds later the photo is asked for again by itself.
  await expect(first.getByText("The photo could not be shown here.")).toBeVisible();
  await expect(first.getByRole("button", { name: "Try showing once.png again" })).toBeVisible();
  await expect(first.getByRole("button", { name: /^Open the photo/ })).toBeVisible({ timeout: 15_000 });
  // The file under it is as wide as the photo: one edge down the bubble.
  await expect.poll(() => first.evaluate(node => {
    const photo = node.querySelector("button[aria-label^='Open the photo']")!.getBoundingClientRect();
    const card = node.querySelector("a[download]")!.parentElement!.getBoundingClientRect();
    return Math.round(Math.abs(photo.right - card.right) + Math.abs(photo.left - card.left));
  }), { message: "the file's card lines up with the photo" }).toBeLessThanOrEqual(1);
  // The one that fails again stays a card until Try again.
  await expect.poll(() => twice, { timeout: 15_000 }).toBe(0);
  await expect(second.getByText("The photo could not be shown here.")).toBeVisible();
  await second.getByRole("button", { name: "Try showing twice.png again" }).click();
  await expect(second.getByRole("button", { name: /^Open the photo/ })).toBeVisible();

  expect(errors()).toEqual([]);
  await ada.close();
});

/* A link that leads nowhere ------------------------------------------------------------------------------------- */

test("a conversation that is not here, or a group only invited to, is said in a Carbon's words", async ({ browser }) => {
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  const missing = `c:dmc-ada::si:nobody-${stamp()}`;
  await page.goto(`/c/${missing}`);
  await expect(page.getByRole("heading", { name: "This conversation is not here" })).toBeVisible();
  const main = page.getByRole("main");
  await expect(main).toContainText(`There is no conversation ${missing} that you are in: the link may be wrong, or you may have left it. Your conversations are in the list.`);
  // The service's hint is for the CLI and HTTP clients: never here.
  await expect(main).not.toContainText(/dm conversations|dm groups|GET \/api|DM does not say/);
  await expect(page.getByRole("link", { name: "Back to your conversations" })).toBeVisible();
  await page.goto(`/c/g:nothing-here-${stamp()}`);
  await expect(page.getByRole("heading", { name: "This group is not here" })).toBeVisible();
  await expect(page.getByRole("main")).not.toContainText(/dm groups|GET \/api/);

  // Bo, invited to Ada's group but not in it yet: the way in is his invitations.
  const group = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name: `Invited ${stamp()}`, member_ids: ["c:dmc-bo"] } });
  const bo = await signedIn(browser, "bo");
  const friend = await bo.newPage();
  const friendErrors = threadErrors(friend);
  await friend.goto(`/c/${group.id}`);
  await expect(friend.getByRole("heading", { name: "You are invited to this group" })).toBeVisible();
  await expect(friend.getByRole("main")).toContainText("Accept the invitation to read its messages and write in it.");
  await friend.getByRole("button", { name: "Open your invitations" }).click();
  await expect(friend.getByRole("dialog", { name: /Invitations/ })).toBeVisible();
  await dmApi("bo", "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/decline`).catch(() => undefined);

  expect(errors()).toEqual([]);
  expect(friendErrors()).toEqual([]);
  await ada.close();
  await bo.close();
});

/* A tap on a phone ---------------------------------------------------------------------------------------------- */

test("on a phone, a tapped message stays under the finger: what follows makes room for its toolbar", async ({ browser }) => {
  const group = await newGroup("Thread taps");
  const mark = stamp();
  let last: Sent | null = null;
  for (let index = 1; index <= 8; index++) last = await send(index % 2 ? "bo" : "ada", group, { message: `Tap ${index} ${mark}: long enough to fill most of the column on a phone, so its toolbar opens under it.` });
  // Ada has read them all: the thread opens at its bottom.
  await dmApi("ada", "POST", `/api/v1/conversations/${encodeURIComponent(group)}/receipts`, { type: "receipts.read", data: { up_to: last!["message-id"] } });
  const ada = await signedIn(browser, "ada", { viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  // A touch screen: nothing raises the on-screen keyboard by itself.
  await expect(row(page, `Tap 8 ${mark}`)).toBeVisible();
  expect(await page.evaluate(() => document.activeElement?.tagName)).not.toBe("TEXTAREA");
  const bubble = row(page, `Tap 7 ${mark}`).locator("[data-sq='surface']").first();
  const next = row(page, `Tap 8 ${mark}`).locator("[data-sq='surface']").first();
  const before = (await bubble.boundingBox())!;
  const nextBefore = (await next.boundingBox())!;
  await page.touchscreen.tap(before.x + 20, before.y + before.height / 2);
  await expect.poll(async () => (await toolbars(page)).map(bar => bar.place)).toEqual(["below"]);
  await page.waitForTimeout(300);
  const after = (await bubble.boundingBox())!;
  expect(Math.abs(after.y - before.y), "the tapped message does not move").toBeLessThanOrEqual(1);
  expect((await next.boundingBox())!.y - nextBefore.y, "the message after it makes room").toBeGreaterThan(20);

  expect(errors()).toEqual([]);
  await ada.close();
});

/* The first message of an empty conversation is news ------------------------------------------------------------ */

test("the log reads out the first message of a conversation that opened empty", async ({ browser }) => {
  const group = await newGroup("Thread first words");
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = threadErrors(page);
  await page.goto(`/c/${group}`);
  await expect(page.getByText(/^This is the beginning of Thread first words/)).toBeVisible();
  // How the log is set to speak at the moment each message is added to it.
  await page.evaluate(() => {
    const log = document.querySelector("[role='log']")!;
    const seen: string[] = [];
    (window as unknown as { __live: string[] }).__live = seen;
    new MutationObserver(records => {
      for (const record of records) for (const node of record.addedNodes) if (node instanceof HTMLElement && node.matches("article[data-message-code]")) seen.push(log.getAttribute("aria-live") ?? "");
    }).observe(log, { childList: true });
  });
  const words = `First words ${stamp()}`;
  await send("bo", group, { message: words });
  await expect(row(page, words)).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { __live: string[] }).__live)).toEqual(["polite"]);

  expect(errors()).toEqual([]);
  await ada.close();
});
