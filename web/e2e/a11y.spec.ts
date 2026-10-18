/**
 * Accessibility: axe-core's WCAG 2.2 A and AA rules on every page, light and dark, desktop and phone, plus the keyboard
 * path through the shell. A violation fails with the rule, the element and how to fix it. (The CSP is bypassed only so
 * axe's script can be injected.) The conversation it checks is its own, made for the run with known content (two
 * Carbons, formatting, a wide code block, a reply, an edit and a deletion), so a result never depends on what other runs
 * left first in the shared list.
 */
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";
import { accountsSession, appConfig, BASE, runAccounts, sameOrigin, signIn } from "./support";

const AXE = join(process.cwd(), "node_modules", "axe-core", "axe.min.js");
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];

test.describe.configure({ mode: "serial" });

let signedIn: BrowserContext;
/** The run's own conversation (a group of the two Carbons), and the words of its first message. */
let conversation: { address: string; first: string };

async function audit(page: Page, label: string) {
  await page.waitForLoadState("domcontentloaded");
  // Measure the page at rest: a panel fading in has its text at partial opacity, which is not what anyone reads.
  await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== "running"), undefined, { timeout: 5_000 }).catch(() => undefined);
  await page.waitForTimeout(500);
  await page.addScriptTag({ path: AXE });
  const result = await page.evaluate(async tags => {
    const axe = (window as unknown as { axe: { run: (context: Document, options: object) => Promise<{ violations: Array<{ id: string; impact: string; help: string; nodes: Array<{ target: string[]; failureSummary: string }> }> }> } }).axe;
    return axe.run(document, { runOnly: { type: "tag", values: tags }, resultTypes: ["violations"] });
  }, TAGS);
  const problems = result.violations.map(violation => `${violation.id} (${violation.impact}): ${violation.help}\n${violation.nodes.slice(0, 4).map(node => `    ${node.target.join(" ")}: ${node.failureSummary.replace(/\s+/g, " ")}`).join("\n")}`);
  mkdirSync("screens",{recursive:true});
  await page.screenshot({path:`screens/${label.replaceAll(/[^a-z0-9]+/gi,"-")}.png`,fullPage:true});
  expect(problems, `${label}\n${problems.join("\n")}`).toEqual([]);
}

async function contextFor(browser: Browser, theme: "light" | "dark", mobile: boolean, storageState?: Awaited<ReturnType<BrowserContext["storageState"]>>) {
  return browser.newContext({ baseURL: BASE, bypassCSP: true, colorScheme: theme, reducedMotion: "reduce", viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 900 }, storageState });
}

/** The DM service through the website's proxy, as the signed-in browser reaches it. */
async function api<T>(context: BrowserContext, method: string, path: string, data?: unknown): Promise<T> {
  const response = await context.request.fetch(`/api${path}`, {
    method,
    headers: sameOrigin({ "Idempotency-Key": crypto.randomUUID(), ...(data === undefined ? {} : { "Content-Type": "application/json" }) }),
    data: data === undefined ? undefined : JSON.stringify(data),
  });
  const body = (await response.json().catch(() => null)) as { data?: T } | null;
  expect(response.ok(), `${method} ${path} answered ${response.status()}: ${JSON.stringify(body)}`).toBe(true);
  return body?.data as T;
}

/** A group of the run's two Carbons with what a conversation shows: formatting, code, a reply, an edit, a deletion. */
async function makeConversation(browser: Browser): Promise<{ address: string; first: string }> {
  const stamp = Date.now().toString(36);
  const friend = await accountsSession(browser, "friend");
  await signIn(await friend.newPage(), runAccounts().friend.email, appConfig.home);
  const group = await api<{ id: string }>(signedIn, "POST", "/api/v1/groups", { type: "create_group", data: { name: `A11y check ${stamp}`, member_ids: [runAccounts().friend.id] } });
  const address = encodeURIComponent(group.id);
  await api(friend, "POST", `/api/v1/groups/${address}/accept`);
  const send = (context: BrowserContext, data: object) => api<{ "message-id": string }>(context, "POST", `/api/v1/conversations/${address}/messages`, { type: "message.create", data });
  const first = `Morning! The **staging** build is green ${stamp}.`;
  await send(friend, { message: `${first}\n\n- \`cargo test\` passes\n- the *docs* site builds\n- notes at https://docs.dm.teamofsilicons.com/start/` });
  const question = await send(signedIn, { message: "Lovely. Could you paste the command?" });
  await api(signedIn, "PATCH", `/api/v1/conversations/${address}/messages/${question["message-id"]}`, { type: "message.updated", data: { message: "Lovely, thank you. Could you paste the exact command you ran?" } });
  await send(friend, { message: "```sh\ncargo test --locked --workspace --all-targets --all-features -- --test-threads=1 --nocapture\n```", reply: { "message-id": question["message-id"] } });
  const mistake = await send(signedIn, { message: "Sent to the wrong conversation" });
  await api(signedIn, "DELETE", `/api/v1/conversations/${address}/messages/${mistake["message-id"]}`);
  await send(friend, { message: "One more thing: the release notes are long, so they wrap over several lines in a narrow window, and that is fine." });
  await friend.close();
  return { address: group.id, first };
}

test.beforeAll(async ({ browser }) => {
  signedIn = await accountsSession(browser, "carbon");
  const page = await signedIn.newPage();
  await signIn(page, runAccounts().carbon.email, appConfig.home);
  await page.close();
  conversation = await makeConversation(browser);
});

test.afterAll(async () => {
  // Ada leaves the run's group, so her list does not keep it (Bo, its other member, stays its owner).
  if (conversation) {
    const group = encodeURIComponent(conversation.address);
    await api(signedIn, "POST", `/api/v1/groups/${group}/owners`, { type: "add_group_owners", data: { member_ids: [runAccounts().friend.id] } }).catch(() => undefined);
    await api(signedIn, "POST", `/api/v1/groups/${group}/leave`).catch(() => undefined);
  }
  await signedIn?.close();
});

for (const theme of ["light", "dark"] as const) {
  for (const mobile of [false, true]) {
    const where = `${theme}, ${mobile ? "phone" : "desktop"}`;
    test(`public pages pass WCAG 2.2 AA (${where})`, async ({ browser }) => {
      const context = await contextFor(browser, theme, mobile);
      const page = await context.newPage();
      for (const path of ["/", "/sign-in", "/sign-in?error=access_denied", "/sign-in?reason=session_ended", "/no/such/page"]) {
        await page.goto(path);
        await audit(page, `${path} (${where})`);
      }
      await context.close();
    });

    test(`workspace pages pass WCAG 2.2 AA (${where})`, async ({ browser }) => {
      const context = await contextFor(browser, theme, mobile, await signedIn.storageState());
      const page = await context.newPage();
      // The chat's sections and a dialog opened by address.
      for (const path of [...appConfig.nav.map(item => item.href), "/c?new=1", "/c?new=group"]) {
        await page.goto(path);
        await audit(page, `${path} (${where})`);
      }
      // The run's own conversation, with everything a conversation shows.
      await page.goto(`/c/${conversation.address}`);
      await expect(page.locator("article[data-message-code]", { hasText: conversation.first.replace(/\*\*/g, "") })).toBeVisible();
      await audit(page, `a conversation (${where})`);
      if (!mobile) {
        // With its details panel open (⌘.), as a Carbon often has it.
        await page.getByRole("main").click({ position: { x: 8, y: 300 } });
        await page.keyboard.press(process.platform === "darwin" ? "Meta+Period" : "Control+Period");
        await expect(page.locator("[data-details-panel]")).toBeVisible();
        await audit(page, `a conversation with its details (${where})`);
        await page.keyboard.press(process.platform === "darwin" ? "Meta+Period" : "Control+Period");
        await expect(page.locator("[data-details-panel]")).toBeHidden();
      }
      // The cheatsheet, opened from outside any field (? would type into the message field).
      await page.getByRole("main").click({ position: { x: 8, y: 300 } });
      await page.keyboard.press("Shift+?");
      await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toBeVisible();
      await audit(page, `the keyboard shortcuts (${where})`);
      await page.keyboard.press("Escape");
      // The ⌘K palette, with something typed (its best match highlighted).
      await page.getByRole("main").click({ position: { x: 8, y: 300 } });
      await page.keyboard.press(process.platform === "darwin" ? "Meta+K" : "Control+K");
      await expect(page.getByRole("dialog", { name: "Search conversations and actions" })).toBeVisible();
      await page.keyboard.type("set");
      await audit(page, `the command palette (${where})`);
      await page.keyboard.press("Escape");
      await page.keyboard.press("Escape");
      // The account menu (a bottom sheet on a phone, at the foot of the list: a phone shows the list on its own page).
      await page.goto(appConfig.home);
      await page.getByRole("button", { name: /^Account menu, / }).click();
      await expect(page.getByRole("menu")).toBeVisible();
      await audit(page, `the account menu (${where})`);
      await page.keyboard.press("Escape");
      // Settings → Keyboard while a shortcut's new keys are being recorded.
      await page.goto("/settings#keyboard");
      const recorder = page.getByRole("button", { name: /^Keys for Command palette/ });
      await recorder.click();
      await expect(recorder).toHaveAttribute("aria-pressed", "true");
      await audit(page, `a shortcut being recorded (${where})`);
      await page.keyboard.press("Escape");
      await context.close();
    });
  }
}

/** One focus stop as the walk measured it. */
interface FocusStop {
  name: string;
  /** Pixels that changed by 3:1 or more with the keyboard on it, and the length of the measured edge. */
  strong: number;
  edge: number;
  /** Silicon UI's ring drawn on top of an edge the control (or its field) draws itself: two indicators. */
  doubled: boolean;
}

/**
 * How much keyboard focus changes the control that has it, as WCAG 1.4.11 measures a focus indicator: each pixel of
 * the control (and 4 px around it; for a field, the edge around it that shows its focus) with the keyboard on it,
 * against the same pixel at rest, transitions off, as a contrast ratio. A 2 px edge all around changes about twice the
 * edge's length of pixels; a fill or a hairline alone changes none by 3:1.
 */
async function focusStop(page: Page): Promise<FocusStop | null> {
  const found = await page.evaluate(() => {
    const element = document.activeElement as HTMLElement | null;
    if (!element || element === document.body) return null;
    const name = `${element.tagName.toLowerCase()} "${(element.getAttribute("aria-label") ?? element.textContent ?? "").trim().slice(0, 40)}"`;
    const edgeOf = (node: Element) => {
      const style = getComputedStyle(node);
      return [style.boxShadow, parseFloat(style.borderTopWidth) > 0 ? style.borderTopColor : "", style.getPropertyValue("--sq-stroke").trim()].join("|");
    };
    const ringOf = (node: Element) => {
      const style = getComputedStyle(node);
      return style.outlineStyle !== "none" && parseFloat(style.outlineWidth) > 0 && !/\/ 0\)|rgba\(0, 0, 0, 0\)|transparent/.test(style.outlineColor);
    };
    // A field shows focus on the edge around it (Silicon UI's fields, the composer's box): measure that.
    const field = element.matches("input, textarea, select, [contenteditable]:not([contenteditable='false'])");
    const candidates = [element, element.parentElement, element.parentElement?.parentElement].filter((node): node is HTMLElement => !!node);
    const focused = candidates.map(edgeOf);
    const ring = ringOf(element);
    element.blur();
    const rest = candidates.map(edgeOf);
    element.focus({ preventScroll: true });
    const changed = candidates.map((_, index) => focused[index] !== rest[index]);
    const measured = field ? candidates.find((_, index) => changed[index]) ?? element : element;
    measured.setAttribute("data-focus-walk", "");
    const box = measured.getBoundingClientRect();
    return { name, ring, edge: changed[0] || (field && changed.some(Boolean)), box: { x: box.x, y: box.y, width: box.width, height: box.height } };
  });
  if (!found) return null;
  const viewport = page.viewportSize()!;
  const { box } = found;
  // The edges of the measured box that are on screen (a list taller than the window shows only some of its edge).
  const left = Math.max(0, box.x - 4);
  const top = Math.max(0, box.y - 4);
  const clip = { x: left, y: top, width: Math.max(1, Math.min(viewport.width, box.x + box.width + 4) - left), height: Math.max(1, Math.min(viewport.height, box.y + box.height + 4) - top) };
  const shownWidth = Math.min(viewport.width, box.x + box.width) - Math.max(0, box.x);
  const shownHeight = Math.min(viewport.height, box.y + box.height) - Math.max(0, box.y);
  const edge = Math.round((box.y >= 0 ? shownWidth : 0) + (box.y + box.height <= viewport.height ? shownWidth : 0) + (box.x >= 0 ? shownHeight : 0) + (box.x + box.width <= viewport.width ? shownHeight : 0));
  const focused = await page.screenshot({ clip });
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  const rest = await page.screenshot({ clip });
  // Back where it was, shown as the keyboard shows it (a key first: :focus-visible), so the walk goes on from here.
  await page.keyboard.press("Shift");
  await page.locator("[data-focus-walk]").evaluate(element => element.removeAttribute("data-focus-walk"));
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
  return { name: found.name, strong, edge, doubled: found.ring && found.edge };
}

/**
 * Tabs through `steps` stops from where the keyboard is, measuring each (focusStop) and putting the keyboard back on it
 * before the next Tab. `within`: stop once the keyboard leaves this element.
 */
async function walkFocus(page: Page, steps: number, within?: string): Promise<{ weak: string[]; doubled: string[] }> {
  const weak: string[] = [];
  const doubled: string[] = [];
  const seen = new Set<string>();
  for (let step = 0; step < steps; step++) {
    await page.keyboard.press("Tab");
    if (within && !(await page.evaluate(selector => !!document.activeElement?.closest(selector), within))) break;
    const handle = await page.evaluateHandle(() => document.activeElement);
    const stop = await focusStop(page);
    await (handle as import("@playwright/test").ElementHandle<HTMLElement>).focus().catch(() => undefined);
    if (!stop || seen.has(stop.name)) continue;
    seen.add(stop.name);
    if (process.env.FOCUS_WALK_DEBUG) console.log(`${stop.strong} of ${stop.edge}${stop.doubled ? " (doubled)" : ""}: ${stop.name}`);
    // WCAG 1.4.11: at least three quarters of its edge's length of pixels change by 3:1 or more.
    if (stop.strong < stop.edge * 0.75) weak.push(`${stop.name} (${stop.strong} of ${stop.edge})`);
    if (stop.doubled) doubled.push(stop.name);
  }
  return { weak, doubled };
}

test("the keyboard reaches everything in the shell, and focus is always visible", async ({ browser }) => {
  const context = await contextFor(browser, "light", false, await signedIn.storageState());
  const page = await context.newPage();
  await page.goto(appConfig.home);
  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Skip to content" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator("main#main")).toBeFocused();
  // Every focus stop shows where it is by a change of at least 3:1 (WCAG 1.4.11; DESIGN.md, Focus and accessibility),
  // and with one indicator: never Silicon UI's ring on top of an edge the control draws itself. Measured with
  // transitions off and tooltips hidden (a tooltip is no focus indicator).
  const quiet = "*, *::before, *::after { transition: none !important; animation: none !important; } [role=tooltip] { display: none !important; }";
  await page.goto(appConfig.home);
  await page.addStyleTag({ content: quiet });
  const shell = await walkFocus(page, 14);
  // The run's own conversation: its header, the message list and the composer (+, the field, Send, the microphone).
  await page.goto(`/c/${conversation.address}`);
  await expect(page.getByRole("textbox", { name: /^Message / })).toBeFocused();
  await page.addStyleTag({ content: quiet });
  await page.locator("main header").first().evaluate(header => {
    header.setAttribute("tabindex", "-1");
    header.focus();
  });
  const thread = await walkFocus(page, 12, "main");
  const weak = [...shell.weak, ...thread.weak];
  const doubled = [...shell.doubled, ...thread.doubled];
  expect(weak, `focus changes less than 3:1 on: ${weak.join(", ")}`).toEqual([]);
  expect(doubled, `Silicon UI's ring drawn over the control's own focus edge on: ${doubled.join(", ")}`).toEqual([]);
  await context.close();
});
