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

test("the keyboard reaches everything in the shell, and focus is always visible", async ({ browser }) => {
  const context = await contextFor(browser, "light", false, await signedIn.storageState());
  const page = await context.newPage();
  await page.goto(appConfig.home);
  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Skip to content" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator("main#main")).toBeFocused();
  // Every focus stop shows where it is: a fill, an edge or an underline that differs from its resting look (compared
  // with transitions off, so a fading fill is not read mid-way).
  await page.goto(appConfig.home);
  await page.addStyleTag({ content: "*, *::before, *::after { transition: none !important; animation: none !important; }" });
  const unseen: string[] = [];
  const ringed: string[] = [];
  for (let step = 0; step < 14; step++) {
    await page.keyboard.press("Tab");
    const visible = await page.evaluate(() => {
      const element = document.activeElement as HTMLElement | null;
      if (!element || element === document.body) return { name: "body", changed: true, ring: false };
      const name = `${element.tagName.toLowerCase()} "${(element.getAttribute("aria-label") ?? element.textContent ?? "").trim().slice(0, 40)}"`;
      // No outline ring on top of the fill or edge a control draws (Silicon UI's ring wins over `outline: none`).
      const own = getComputedStyle(element);
      const ring = own.outlineStyle !== "none" && parseFloat(own.outlineWidth) > 0 && !/\/ 0\)|rgba\(0, 0, 0, 0\)|transparent/.test(own.outlineColor);
      // Arc draws focus on the control, or on what paints it: a highlight inside it, the field around it
      // (:focus-within) or the surface right after it (Add filter). Look at all of them.
      const painted = [element, element.parentElement, element.parentElement?.parentElement, element.nextElementSibling, ...Array.from(element.children).slice(0, 3)].filter((node): node is Element => !!node);
      const snapshot = () => painted.map(node => {
        const style = getComputedStyle(node);
        return [style.backgroundColor, style.borderColor, style.boxShadow, style.color, style.textDecorationLine, style.outlineStyle, style.transform, style.opacity].join("|");
      }).join("/");
      const now = snapshot();
      element.blur();
      const rest = snapshot();
      element.focus({ preventScroll: true });
      return { name, changed: now !== rest, ring };
    });
    if (!visible.changed) unseen.push(visible.name);
    if (visible.ring) ringed.push(visible.name);
  }
  expect(unseen, `focus not visible on: ${unseen.join(", ")}`).toEqual([]);
  expect(ringed, `a ring drawn over the focus look on: ${ringed.join(", ")}`).toEqual([]);
  await context.close();
});
