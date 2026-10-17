/**
 * Voice end to end against the local chat stack (scripts/chat-stack/up.sh): Chrome's fake microphone plays a voiced
 * tone, the Briefcase stand-in keeps the voice note, the Deepgram stand-in transcribes it ("This is a voice note from
 * the stand-in.") and renders speech. Every test makes its own data (a new group, or messages with a unique marker) and
 * asserts only on it, because other runs use the same stack at the same time.
 *
 *   a voice note: recorded with ⌘⇧M, sent with Enter, heard by the other Carbon with its transcript
 *   dictation: ⌘⇧D, Enter, the stand-in's words in the field
 *   Listen on a Silicon's words; a spoken message: "Preparing audio…", then its waveform; Listen mode speaks what arrives
 *   Settings → Voice: choosing Scout's voice is what the service says
 *   axe on the voice surfaces
 */
import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";
import { BASE, watchErrors } from "./support";

const HELPER = join(process.cwd(), "..", "scripts", "chat-stack", "identities.mjs");
const DM_API = `http://127.0.0.1:${process.env.API_PORT || 4521}`;
const AXE = join(process.cwd(), "node_modules", "axe-core", "axe.min.js");
const MOD = process.platform === "darwin" ? "Meta" : "Control";
const STAND_IN_WORDS = "This is a voice note from the stand-in.";

/* The fake microphone's sound: six seconds of a voiced tone in syllables (silence would transcribe to nothing). */
const FAKE_WAV = join(process.cwd(), "test-results", `voice-fake-mic-${process.env.E2E_PORT || 4530}.wav`);
function writeFakeVoice(path: string): void {
  const rate = 48_000;
  const samples = rate * 6;
  const bytes = Buffer.alloc(44 + samples * 2);
  bytes.write("RIFF", 0);
  bytes.writeUInt32LE(36 + samples * 2, 4);
  bytes.write("WAVEfmt ", 8);
  bytes.writeUInt32LE(16, 16);
  bytes.writeUInt16LE(1, 20);
  bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(rate, 24);
  bytes.writeUInt32LE(rate * 2, 28);
  bytes.writeUInt16LE(2, 32);
  bytes.writeUInt16LE(16, 34);
  bytes.write("data", 36);
  bytes.writeUInt32LE(samples * 2, 40);
  for (let index = 0; index < samples; index++) {
    const t = index / rate;
    const syllable = Math.floor(t * 4) % 5 === 4 ? 0.05 : 1;
    const pitch = 140 + 25 * Math.sin(2 * Math.PI * 0.7 * t);
    const value = (0.55 * Math.sin(2 * Math.PI * pitch * t) + 0.25 * Math.sin(4 * Math.PI * pitch * t)) * (0.5 + 0.5 * Math.sin(5 * Math.PI * t)) * syllable * 0.8;
    bytes.writeInt16LE(Math.round(Math.max(-1, Math.min(1, value)) * 32767), 44 + index * 2);
  }
  mkdirSync(join(process.cwd(), "test-results"), { recursive: true });
  writeFileSync(path, bytes);
}
writeFakeVoice(FAKE_WAV);

test.use({
  launchOptions: { args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream", `--use-file-for-fake-audio-capture=${FAKE_WAV}`, "--autoplay-policy=no-user-gesture-required"] },
});
test.describe.configure({ mode: "serial" });

/* The stack's identities: a website cookie, a DM token, the CLI as a Silicon. -------------------------------------- */

const helper = (...args: string[]) => execFileSync("node", [HELPER, ...args], { encoding: "utf8" });
const tokenOf = (who: string) => helper("token", who, "--raw").trim();

async function signedIn(browser: Browser, who: string, options: Parameters<Browser["newContext"]>[0] = {}): Promise<BrowserContext> {
  const cookie = JSON.parse(helper("cookie", who)) as { name: string; value: string; httpOnly?: boolean; secure?: boolean; sameSite?: "Lax" | "Strict" | "None"; expires?: number };
  const context = await browser.newContext({ baseURL: BASE, permissions: ["microphone"], ...options });
  await context.addCookies([{ name: cookie.name, value: cookie.value, url: BASE, httpOnly: cookie.httpOnly ?? true, secure: false, sameSite: cookie.sameSite ?? "Lax" }]);
  return context;
}

async function dmApi<T>(who: string, method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${DM_API}${path}`, {
    method,
    headers: { Authorization: `Bearer ${tokenOf(who)}`, "Content-Type": "application/json", "Idempotency-Key": crypto.randomUUID() },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`${method} ${path} → ${response.status} ${text}`);
  return (text ? (JSON.parse(text) as { data: T }).data : null) as T;
}

/** A Silicon sends a message from the CLI (through its relay), like a Silicon does. */
function scoutSays(args: string[]): void {
  execFileSync("node", [HELPER, "dm", "scout", "--", "messages", "send", "c:dmc-ada", ...args, "--wait"], { stdio: ["ignore", "pipe", "pipe"] });
}

const stamp = () => `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;

/** What the shared <audio> element is doing. */
const player = (page: Page) =>
  page.evaluate(() => {
    const audio = document.querySelector<HTMLAudioElement>("audio[data-dm-player]");
    return audio ? { track: audio.dataset.track ?? null, time: audio.currentTime, paused: audio.paused } : null;
  });

async function expectPlaying(page: Page, trackEnding: string | RegExp): Promise<void> {
  await expect.poll(async () => {
    const state = await player(page);
    if (!state?.track) return "nothing";
    const matches = typeof trackEnding === "string" ? state.track.endsWith(trackEnding) : trackEnding.test(state.track);
    return matches && state.time > 0.2 ? "advancing" : `${state.track} at ${state.time.toFixed(2)}`;
  }, { timeout: 30_000, message: "the audio of that message plays (currentTime advances)" }).toBe("advancing");
}

/**
 * axe-core's WCAG 2.2 A and AA rules on the voice surfaces on screen (`[data-voice-ui]`: players, Listen mode, the
 * recording bar, the voice pickers and settings); e2e/a11y.spec.ts audits whole pages.
 */
async function audit(page: Page, label: string): Promise<void> {
  await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== "running" || animation.effect?.getTiming().iterations === Infinity), undefined, { timeout: 5_000 }).catch(() => undefined);
  await page.addScriptTag({ path: AXE });
  const result = await page.evaluate(async () => {
    const axe = (window as unknown as { axe: { run: (context: object, options: object) => Promise<{ violations: Array<{ id: string; impact: string; help: string; nodes: Array<{ target: string[]; failureSummary: string }> }> }> } }).axe;
    return axe.run({ include: [["[data-voice-ui]"]] }, { runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"] }, resultTypes: ["violations"] });
  });
  const problems = result.violations.map(violation => `${violation.id} (${violation.impact}): ${violation.help}\n${violation.nodes.slice(0, 4).map(node => `    ${node.target.join(" ")}: ${node.failureSummary.replace(/\s+/g, " ")}`).join("\n")}`);
  expect(problems, `${label}\n${problems.join("\n")}`).toEqual([]);
}

/* Voice notes and dictation, in a group of the run's own ------------------------------------------------------------ */

test("a voice note: recorded, uploaded, transcribed, heard by the other Carbon; dictation types the words", async ({ browser }) => {
  const name = `Voice check ${stamp()}`;
  const group = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name, member_ids: ["c:dmc-bo"] } });
  await dmApi("bo", "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/accept`);

  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  await page.goto(`/c/${group.id}`);
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(field).toBeVisible();

  // ⌘⇧M in the field records; the bar covers the composer with the time, a meter, ✕ and ✓ (which has the keyboard).
  const caption = `Listen to this ${stamp()}`;
  await field.fill(caption);
  await field.press(`${MOD}+Shift+M`);
  const recording = page.getByRole("group", { name: "Recording a voice message" });
  await expect(recording).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop and send the voice message" })).toBeFocused();
  await expect(recording.getByRole("timer")).toHaveText(/0:0[1-9]/, { timeout: 10_000 });
  // The bar covers the composer, so the keyboard stays in it: Tab goes between ✕ and ✓.
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Cancel the voice message" })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Stop and send the voice message" })).toBeFocused();
  await page.waitForTimeout(500);
  // Enter stops and sends: the tray takes it (uploading, transcribing), then the message goes with its transcript.
  await page.keyboard.press("Enter");
  await expect(recording).toBeHidden();
  const sent = page.locator("article[data-message-code]", { hasText: caption });
  await expect(sent).toBeVisible();
  await expect(sent).toContainText(STAND_IN_WORDS, { timeout: 30_000 });
  type Stored = { items: Array<{ message: string; voice: { url: string; duration_milliseconds: number } | null; voice_transcript: string | null }> };
  const storedNote = async () => (await dmApi<Stored>("ada", "GET", `/api/v1/conversations/${encodeURIComponent(group.id)}/messages?limit=5`)).items.find(item => item.message === caption);
  await expect.poll(async () => (await storedNote())?.voice?.url ?? null, { timeout: 30_000, message: "the message carries the voice note's Briefcase link" }).toMatch(/\/api\/v1\/public\//);
  const note = await storedNote();
  expect(note?.voice?.duration_milliseconds ?? 0).toBeGreaterThan(1000);
  expect(note?.voice_transcript).toBe(STAND_IN_WORDS);

  // Bo has it live: the voice bubble with its transcript, and it plays.
  const bo = await signedIn(browser, "bo");
  const friend = await bo.newPage();
  const friendErrors = watchErrors(friend);
  await friend.goto(`/c/${group.id}`);
  const received = friend.locator("article[data-message-code]", { hasText: caption });
  await expect(received).toContainText(STAND_IN_WORDS);
  const play = received.getByRole("button", { name: /^Play Ada's voice message/ });
  await expect(play).toBeVisible();
  const code = await received.getAttribute("data-message-code");
  await play.click();
  await expectPlaying(friend, `|${code}`);
  await expect(received.getByRole("button", { name: /^Pause Ada's voice message/ })).toBeVisible();
  // The speed is offered while it plays (and is Bo's default from then on).
  await received.getByRole("button", { name: "1.5×" }).click();
  await expect.poll(() => friend.evaluate(() => document.querySelector<HTMLAudioElement>("audio[data-dm-player]")?.playbackRate)).toBe(1.5);
  await received.getByRole("button", { name: "1×", exact: true }).click();
  await audit(friend, "a conversation with a voice note, playing");

  // Dictation: ⌘⇧D, Enter, the words land in the field (nothing is uploaded or sent).
  await field.click();
  await field.fill("");
  await field.press(`${MOD}+Shift+D`);
  const dictating = page.getByRole("group", { name: "Dictating" });
  await expect(dictating).toBeVisible();
  await expect(dictating.getByRole("timer")).toHaveText(/0:0[1-9]/, { timeout: 10_000 });
  await page.waitForTimeout(500);
  await page.keyboard.press("Enter");
  await expect(field).toHaveValue(STAND_IN_WORDS, { timeout: 30_000 });
  await expect(field).toBeFocused();
  // Esc cancels a recording: nothing changes.
  await field.fill("");
  await field.press(`${MOD}+Shift+M`);
  await expect(recording).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(recording).toBeHidden();
  await expect(field).toHaveValue("");
  // The shortcut that started it stops and sends it too.
  const second = `Again ${stamp()}`;
  await field.fill(second);
  await field.press(`${MOD}+Shift+M`);
  await expect(recording.getByRole("timer")).toHaveText(/0:0[1-9]/, { timeout: 10_000 });
  await page.keyboard.press(`${MOD}+Shift+M`);
  await expect(recording).toBeHidden();
  await expect(page.locator("article[data-message-code]", { hasText: second }).getByRole("button", { name: /^Play your voice message/ })).toBeVisible({ timeout: 30_000 });

  expect(errors).toEqual([]);
  expect(friendErrors).toEqual([]);
  await ada.close();
  await bo.close();
});

/* What a failure does not take away ------------------------------------------------------------------------------- */

test("a failed transcription keeps what was said (Try again sends it again); a failed upload is tried again from its toast", async ({ browser }) => {
  const group = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name: `Voice failures ${stamp()}`, member_ids: ["c:dmc-bo"] } });
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  await page.goto(`/c/${group.id}`);
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(field).toBeVisible();

  // Dictation: the service is unavailable for the first transcription only (this page's requests, nobody else's).
  let refuse = true;
  let asked = 0;
  await page.route(url => url.pathname.endsWith("/api/v1/speech/transcriptions"), async route => {
    asked += 1;
    if (!refuse) return route.continue();
    return route.fulfill({ status: 503, headers: { "content-type": "application/json", "retry-after": "5" }, body: JSON.stringify({ type: "error", data: { error: { code: "speech_unavailable", message: "The speech provider did not answer in time.", hint: "Try again in a moment." } } }) });
  });
  await field.click();
  await field.press(`${MOD}+Shift+D`);
  const dictating = page.getByRole("group", { name: "Dictating" });
  await expect(dictating.getByRole("timer")).toHaveText(/0:0[1-9]/, { timeout: 10_000 });
  await page.keyboard.press("Enter");
  await expect(page.getByRole("alert").filter({ hasText: "The speech provider did not answer in time." })).toBeVisible();
  await expect(page.getByRole("button", { name: "Try again" })).toBeFocused();
  refuse = false;
  await page.keyboard.press("Enter");
  await expect(field).toHaveValue(STAND_IN_WORDS, { timeout: 30_000 });
  expect(asked, "the same recording was sent again, nothing was recorded anew").toBe(2);

  // A voice message whose upload keeps failing: three tries, then the toast's Try again uploads the same recording.
  let failUploads = true;
  await page.route(url => url.pathname === "/api/uploads", async route => {
    if (route.request().method() !== "POST" || !failUploads) return route.continue();
    return route.fulfill({ status: 502, headers: { "content-type": "application/json" }, body: JSON.stringify({ error: { code: "briefcase_unavailable", message: "Briefcase did not answer.", hint: "Try again in a moment." } }) });
  });
  const caption = `Kept ${stamp()}`;
  await field.fill(caption);
  await field.press(`${MOD}+Shift+M`);
  await expect(page.getByRole("group", { name: "Recording a voice message" }).getByRole("timer")).toHaveText(/0:0[1-9]/, { timeout: 10_000 });
  await page.keyboard.press("Enter");
  await expect(page.getByText("The voice message was not uploaded")).toBeVisible({ timeout: 20_000 });
  failUploads = false;
  await page.getByRole("button", { name: "Try again" }).click();
  const sent = page.locator("article[data-message-code]", { hasText: caption });
  await expect(sent.getByRole("button", { name: /^Play your voice message/ })).toBeVisible({ timeout: 30_000 });
  expect(errors).toEqual([]);
  await ada.close();
});

test("with uploads off the microphone dictates and says why; with speech off listening says why", async ({ browser }) => {
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  // This page's own answers only: the website without Briefcase.
  await page.route(url => url.pathname === "/api/uploads/config", route => route.fulfill({ status: 200, headers: { "content-type": "application/json" }, body: JSON.stringify({ enabled: false, reason: "Uploads are not set up on this website.", max_bytes: 0 }) }));
  await page.goto("/c/c:dmc-ada::si:dmc-scout");
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(field).toBeVisible();
  await expect(page.getByRole("button", { name: "Dictate (voice messages need uploads, which are off here)" })).toBeVisible();
  await field.click();
  await field.press(`${MOD}+Shift+M`);
  await expect(page.getByText("Uploads are not set up on this website. Dictation still works")).toBeVisible();
  await expect(page.getByRole("group", { name: "Recording a voice message" })).toHaveCount(0);

  // The DM service without a speech provider: Listen mode is off and says why; samples cannot play.
  await page.unroute(url => url.pathname === "/api/uploads/config");
  const speechOff = async (route: import("@playwright/test").Route) => {
    const response = await route.fetch();
    const body = (await response.json()) as { data?: { speech?: { enabled?: boolean }; enabled?: boolean } };
    if (body.data?.speech) body.data.speech.enabled = false;
    if (route.request().url().endsWith("/api/v1/voices") && body.data) body.data.enabled = false;
    await route.fulfill({ response, json: body });
  };
  await page.route(url => url.pathname === "/api/api/v1/me" || url.pathname === "/api/api/v1/voices", speechOff);
  await page.reload();
  await expect(page.getByRole("button", { name: /^Listen mode \(Speech is not set up on this DM service/ })).toBeVisible();
  await page.goto("/settings#voice");
  await expect(page.getByText("Speech is not set up on this DM service, so listening and dictation are off. The voices can still be chosen")).toBeVisible();
  await expect(page.getByRole("button", { name: /^Play Mark's sample \(Speech is not set up/ }).first()).toBeVisible();
  expect(errors).toEqual([]);
  await ada.close();
});

/* Listening to Silicons --------------------------------------------------------------------------------------------- */

test("Listen plays a Silicon's words; a spoken message prepares, then shows its waveform; Listen mode speaks what arrives", async ({ browser }) => {
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  await page.goto("/c/c:dmc-ada::si:dmc-scout");
  await expect(page.getByRole("textbox", { name: /^Message / })).toBeVisible();

  // Listen on a text message (the message menu, or Space on a selected message): spoken in Scout's voice.
  const words = `Listen check ${stamp()}`;
  scoutSays(["--text", `${words}, the build is green.`]);
  const text = page.locator("article[data-message-code]", { hasText: words });
  await expect(text).toBeVisible();
  const textCode = await text.getAttribute("data-message-code");
  await text.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Listen" }).click();
  await expectPlaying(page, `|${textCode}`);

  // A spoken message: its words show at once, "Preparing audio…" while the service renders, then the waveform.
  await page.evaluate(() => {
    const seen = window as unknown as { __preparing?: string[] };
    seen.__preparing = [];
    new MutationObserver(() => {
      for (const article of document.querySelectorAll("article[data-message-code]")) {
        if (article.textContent?.includes("Preparing audio…")) seen.__preparing!.push(article.textContent);
      }
    }).observe(document.body, { subtree: true, childList: true, characterData: true });
  });
  const spoken = `Speech check ${stamp()}`;
  const long = Array.from({ length: 60 }, (_, index) => `step ${index + 1} is done`).join(", ");
  const sending = spawn("node", [HELPER, "dm", "scout", "--", "messages", "send", "c:dmc-ada", "--speak", `[warmly] Hi Ada, ${spoken}. ${long}.`, "--dangerously-send-long-message", "--wait"], { stdio: "ignore" });
  const sent = new Promise(resolve => sending.once("exit", resolve));
  const speech = page.locator("article[data-message-code]", { hasText: spoken });
  await expect(speech).toBeVisible({ timeout: 30_000 });
  await expect(speech.getByRole("button", { name: /^Play Scout's spoken message, \d+:\d\d/ })).toBeVisible({ timeout: 30_000 });
  await expect(speech.getByRole("slider", { name: /^Position in Scout's spoken message/ }).locator("svg")).toBeVisible();
  await expect(speech).not.toContainText("Preparing audio…");
  expect(await page.evaluate(marker => ((window as unknown as { __preparing?: string[] }).__preparing ?? []).some(text => text.includes(marker)), spoken), "it said Preparing audio… while the service rendered it").toBe(true);
  await sent;
  // Seeking by keyboard on its waveform once it plays.
  await speech.getByRole("button", { name: /^Play Scout's spoken message/ }).click();
  const speechCode = await speech.getAttribute("data-message-code");
  await expectPlaying(page, `|${speechCode}`);
  const slider = speech.getByRole("slider", { name: /^Position in Scout's spoken message/ });
  await slider.focus();
  const before = (await player(page))!.time;
  for (let step = 0; step < 3; step++) await page.keyboard.press("ArrowRight");
  await expect.poll(async () => (await player(page))?.time ?? 0).toBeGreaterThan(before + 10);
  await page.keyboard.press("Space");
  await expect(speech.getByRole("button", { name: /^Play Scout's spoken message/ })).toBeVisible();

  // Listen mode: turned on in the header (it is pressed), then what Scout says next is spoken without a click.
  const toggle = page.getByRole("button", { name: "Listen mode" });
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
  const arriving = `Listen mode check ${stamp()}`;
  scoutSays(["--text", `${arriving}, ${Array.from({ length: 10 }, (_, index) => `point ${index + 1} still holds`).join(", ")}.`, "--dangerously-send-long-message"]);
  const next = page.locator("article[data-message-code]", { hasText: arriving });
  await expect(next).toBeVisible();
  const nextCode = await next.getAttribute("data-message-code");
  await expectPlaying(page, `|${nextCode}`);
  await expect(page.getByRole("button", { name: "Stop speaking" })).toBeVisible();
  await audit(page, "a conversation with spoken messages, Listen mode speaking");
  // It holds its tongue while the Carbon writes, and goes on a moment after the last keystroke.
  const field = page.getByRole("textbox", { name: /^Message / });
  await field.click();
  await page.keyboard.type("Thinking");
  await expect(page.getByRole("status").filter({ hasText: "Paused while you write" })).toBeVisible();
  await expect.poll(async () => (await player(page))?.paused).toBe(true);
  await field.fill("");
  await expect.poll(async () => (await player(page))?.paused, { timeout: 10_000 }).toBe(false);
  await expect(page.getByRole("status").filter({ hasText: "Speaking…" })).toBeVisible();
  await page.getByRole("button", { name: "Stop speaking" }).click();
  await expect.poll(async () => (await player(page))?.track ?? null).toBeNull();
  // A spoken message waits for its rendering, then is spoken in its own voice.
  const said = `Listen mode speech ${stamp()}`;
  scoutSays(["--speak", `[warmly] ${said}, all done.`]);
  const spokenNext = page.locator("article[data-message-code]", { hasText: said });
  await expect(spokenNext).toBeVisible();
  await expectPlaying(page, `|${await spokenNext.getAttribute("data-message-code")}`);
  await page.getByRole("button", { name: "Stop speaking" }).click();

  // A second tab on this conversation with Listen mode on: one tab speaks, the one where it was turned on last. The
  // other says so, and Listen here brings it back.
  const other = await ada.newPage();
  const otherErrors = watchErrors(other);
  await other.goto("/c/c:dmc-ada::si:dmc-scout");
  await other.getByRole("button", { name: "Listen mode" }).click();
  await expect(page.getByRole("status").filter({ hasText: "On in another tab" })).toBeVisible();
  const twice = `Two tabs ${stamp()}`;
  scoutSays(["--text", `${twice}, ${Array.from({ length: 8 }, (_, index) => `point ${index + 1} holds`).join(", ")}.`, "--dangerously-send-long-message"]);
  const twiceRow = other.locator("article[data-message-code]", { hasText: twice });
  await expect(twiceRow).toBeVisible();
  await expectPlaying(other, `|${await twiceRow.getAttribute("data-message-code")}`);
  expect((await player(page))?.track ?? null, "the other tab stays quiet").toBeNull();
  await page.getByRole("button", { name: /^Listen here/ }).click();
  await expect(other.getByRole("status").filter({ hasText: "On in another tab" })).toBeVisible();
  await expect.poll(async () => (await player(other))?.track ?? null, { message: "the tab that gave way stops" }).toBeNull();
  expect(otherErrors).toEqual([]);
  await other.close();

  // A message deleted while it is spoken stops at once: its words are gone for everyone.
  const doomed = `Deleted ${stamp()}`;
  scoutSays(["--text", `${doomed}, ${Array.from({ length: 12 }, (_, index) => `sentence ${index + 1} keeps going`).join(", ")}.`, "--dangerously-send-long-message"]);
  const doomedRow = page.locator("article[data-message-code]", { hasText: doomed });
  await expect(doomedRow).toBeVisible();
  const doomedCode = (await doomedRow.getAttribute("data-message-code"))!;
  await expectPlaying(page, `|${doomedCode}`);
  helper("dm", "scout", "--", "messages", "delete", "c:dmc-ada::si:dmc-scout", doomedCode);
  await expect.poll(async () => (await player(page))?.track ?? null, { timeout: 15_000, message: "the deleted message stops playing" }).toBeNull();

  // ⌘⇧L turns it off again.
  await page.keyboard.press(`${MOD}+Shift+L`);
  await expect(toggle).toHaveAttribute("aria-pressed", "false");

  // What was typed is gone from the shared stack too.
  await dmApi("ada", "DELETE", "/api/v1/conversations/c:dmc-ada::si:dmc-scout/draft").catch(() => undefined);
  expect(errors).toEqual([]);
  await ada.close();
});

/* Settings → Voice ------------------------------------------------------------------------------------------------- */

test("Settings → Voice: a sample plays, Scout's voice is chosen and reset, and the service says so", async ({ browser }) => {
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  // A link names the Silicon: /settings?silicon=si:dmc-scout#voice opens on Scout (Ada looks after two).
  await page.goto("/settings?silicon=si:dmc-scout#voice");
  await expect(page.getByRole("combobox", { name: "Your Silicon" })).toContainText("Scout");
  const voices = page.getByRole("radiogroup", { name: "Scout's voice" });
  await expect(voices).toBeVisible();
  await audit(page, "Settings → Voice");

  // ▶ plays the sample line in that voice.
  await page.getByRole("button", { name: "Play Hope's sample" }).first().click();
  await expectPlaying(page, /^sample\|hope\|/);

  const was = await dmApi<{ voice: { key: string }; chosen: boolean }>("ada", "GET", "/api/v1/voices/si:dmc-scout");
  const pick = was.voice.key === "brittney" ? "Mark" : "Brittney";
  await voices.getByRole("radio", { name: new RegExp(`^${pick}`) }).click();
  await expect.poll(async () => (await dmApi<{ voice: { name: string } }>("ada", "GET", "/api/v1/voices/si:dmc-scout")).voice.name, { timeout: 10_000 }).toBe(pick);
  const chosen = await dmApi<{ voice: { key: string; name: string }; chosen: boolean; chosen_by: { id: string } | null }>("ada", "GET", "/api/v1/voices/si:dmc-scout");
  expect(chosen.chosen).toBe(true);
  expect(chosen.chosen_by?.id).toBe("c:dmc-ada");

  // Back to the default, as it was: Reset to default sits beside Scout's name (it appears without moving anything).
  await page.getByRole("region", { name: "Scout's voice" }).getByRole("button", { name: "Reset to default" }).click();
  await expect.poll(async () => (await dmApi<{ chosen: boolean }>("ada", "GET", "/api/v1/voices/si:dmc-scout")).chosen, { timeout: 10_000 }).toBe(false);
  if (was.chosen) await dmApi("ada", "PUT", "/api/v1/voices/si:dmc-scout", { type: "voice.set", data: { voice: was.voice.key } });

  await page.goto("/settings#listening");
  await expect(page.getByRole("switch", { name: "Play spoken messages when they arrive" })).toBeVisible();
  await expect(page.getByRole("group", { name: "Speed" })).toBeVisible();
  await audit(page, "Settings → Listening");
  expect(errors).toEqual([]);
  await ada.close();
});
