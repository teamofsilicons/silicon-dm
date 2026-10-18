/**
 * Voice end to end against the local chat stack (scripts/chat-stack/up.sh): Chrome's fake microphone plays a voiced
 * tone, the Briefcase stand-in keeps the voice note, the Deepgram stand-in transcribes it ("This is a voice note from
 * the stand-in.") and renders speech. Every test makes its own data (a new group, or messages with a unique marker) and
 * asserts only on it, because other runs use the same stack at the same time.
 *
 *   a voice note: recorded with ⌘⇧M, sent with Enter (waiting in the outbox it already plays), heard by the other
 *     Carbon with its transcript
 *   dictation: ⌘⇧D, Enter, the stand-in's words in the field; failures in a Carbon's words, the recording kept
 *   an open edit: a voice message waits (the microphone dictates into the edit); a voice message on a link elsewhere is
 *     never fetched by the page (CSP), and shows without errors
 *   Listen on a Silicon's words; a spoken message: "Preparing audio…", then its waveform; Listen mode speaks what
 *     arrives (in a group of the run's own with Scout, so nothing another run says is spoken there)
 *   Settings → Voice: choosing Scout's voice is what the service says
 *   a spoken message with a written caption: the caption is the message, then the player, then the spoken words
 *   a voice message cut off by a reload is kept on disk (OPFS) and offered when the conversation opens again
 *   a phone: Change opens the voices as a sheet; a playing spoken message keeps its voice's name (one speed button)
 *   Listen mode only where a Silicon takes part (the palette leaves it out elsewhere, and ⌘⇧L says why)
 *   listening and playing move nothing in the thread; keyboard focus on the voice controls changes them by 3:1 or more
 *   axe on the voice surfaces
 */
import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Browser, type BrowserContext, type Locator, type Page } from "@playwright/test";
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
function scoutSays(conversation: string, args: string[]): void {
  execFileSync("node", [HELPER, "dm", "scout", "--", "messages", "send", conversation, ...args, "--wait"], { stdio: ["ignore", "pipe", "pipe"] });
}

/**
 * A group of Ada's with Scout in it (her own Silicon joins at once), for this run only: what Scout says there is heard
 * by this test alone, whatever other runs say to Ada meanwhile.
 */
async function groupWithScout(label: string): Promise<string> {
  const group = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name: `${label} ${stamp()}`, member_ids: ["si:dmc-scout"] } });
  return group.id;
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
 * How much keyboard focus changes a control, as WCAG 1.4.11 measures a focus indicator (the same measure as
 * e2e/thread.spec.ts): each pixel of the control and 4 px around it, focused against at rest (transitions off), as a
 * contrast ratio; at least three quarters of its edge's length of pixels must change by 3:1 or more.
 */
async function expectFocusSeen(page: Page, name: string, target: Locator): Promise<void> {
  await page.addStyleTag({ content: "*, *::before, *::after { transition: none !important; animation: none !important; }" });
  await target.scrollIntoViewIfNeeded();
  await page.mouse.move(1, 1);
  const box = (await target.boundingBox())!;
  const viewport = page.viewportSize()!;
  const x = Math.max(0, box.x - 4);
  const y = Math.max(0, box.y - 4);
  const clip = { x, y, width: Math.min(viewport.width - x, box.width + 8), height: Math.min(viewport.height - y, box.height + 8) };
  await page.evaluate(() => (document.activeElement instanceof HTMLElement ? document.activeElement.blur() : undefined));
  const rest = await page.screenshot({ clip });
  // A key first, so the browser shows focus as it does for the keyboard (:focus-visible).
  await page.keyboard.press("Shift");
  await target.focus();
  expect(await target.evaluate(element => element.matches(":focus-visible")), `${name}: the keyboard is on it`).toBe(true);
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
  const edge = Math.round(2 * (box.width + box.height));
  expect(strong, `${name}: pixels that change by 3:1 or more with keyboard focus (WCAG 1.4.11), against an edge of ${edge} px`).toBeGreaterThanOrEqual(edge * 0.75);
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
  // The bar covers the field: it shows the words that go with the voice message, and ✓ says it sends them.
  const stopAndSend = page.getByRole("button", { name: "Stop and send the voice message with your words" });
  await expect(stopAndSend).toBeFocused();
  await expect(recording).toContainText(`With your words: “${caption}”`);
  await expect(recording.getByRole("timer")).toHaveText(/0:0[1-9]/, { timeout: 10_000 });
  // The bar covers the composer, so the keyboard stays in it: Tab goes between ✕ and ✓.
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Cancel the voice message" })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(stopAndSend).toBeFocused();
  await expectFocusSeen(page, "the recorder's Cancel", page.getByRole("button", { name: "Cancel the voice message" }));
  await stopAndSend.focus();
  await page.waitForTimeout(500);
  // The send is held for a moment (this page's request only), to see the message wait in the outbox as itself. Once let
  // go, every send passes straight through.
  let letGo: () => void = () => undefined;
  const held = new Promise<void>(resolve => {
    letGo = resolve;
  });
  await page.route(url => url.pathname.endsWith(`/conversations/${encodeURIComponent(group.id)}/messages`), async route => {
    if (route.request().method() === "POST") await held;
    await route.continue();
  });
  // Enter stops and sends: the tray takes it (uploading, transcribing), then the message goes with its transcript.
  await page.keyboard.press("Enter");
  await expect(recording).toBeHidden();
  // Waiting in the outbox, it is already a voice message that plays (from the recording), with its words.
  const waiting = page.locator("article[data-pending]", { hasText: caption });
  await expect(waiting.getByRole("button", { name: /^Play the voice message/ })).toBeVisible({ timeout: 30_000 });
  await expect(waiting).toContainText(STAND_IN_WORDS);
  await expect(waiting).not.toContainText("Voice message");
  letGo();
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
  await expectFocusSeen(friend, "a voice message's Play", play);
  await expectFocusSeen(friend, "a voice message's waveform", received.getByRole("slider", { name: /^Position in Ada's voice message/ }));
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
  // Said in a Carbon's words (the service's are for Silicons), with how long to wait.
  await expect(page.getByRole("alert").filter({ hasText: "The recording could not be turned into words: the transcription did not answer in time." })).toContainText("Try again in 5 seconds.");
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
  await page.route(url => url.pathname === "/api/uploads/config", route => route.fulfill({ status: 200, headers: { "content-type": "application/json" }, body: JSON.stringify({ enabled: false, reason: "Uploads are not set up on this DM website yet.", max_bytes: 0 }) }));
  await page.goto("/c/c:dmc-ada::si:dmc-scout");
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(field).toBeVisible();
  await expect(page.getByRole("button", { name: "Dictate (voice messages need uploads, which are off here)" })).toBeVisible();
  await field.click();
  await field.press(`${MOD}+Shift+M`);
  await expect(page.getByText("Uploads are not set up on this DM website yet. Dictation still works")).toBeVisible();
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

test("a voice message waits for an open edit (it would go as a new message); one on a link elsewhere plays without errors", async ({ browser }) => {
  const group = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name: `Voice edits ${stamp()}`, member_ids: ["c:dmc-bo"] } });
  await dmApi("bo", "POST", `/api/v1/groups/${encodeURIComponent(group.id)}/accept`);
  const mine = `Edit me ${stamp()}`;
  await dmApi("ada", "POST", `/api/v1/conversations/${encodeURIComponent(group.id)}/messages`, { type: "message.create", data: { message: mine } });
  // A voice message whose link is not a Briefcase file: the page may not read it (connect-src 'self'), so it is not
  // fetched for a waveform; it shows its words and plays through the audio element when asked.
  const elsewhere = `From elsewhere ${stamp()}`;
  await dmApi("bo", "POST", `/api/v1/conversations/${encodeURIComponent(group.id)}/messages`, { type: "message.create", data: { message: elsewhere, voice: { permanent_url: "https://cdn.example.com/notes/hello.ogg", content_type: "audio/ogg", duration_milliseconds: 4000 }, voice_transcript: "Hello from somewhere else." } });

  const ada = await signedIn(browser, "ada");
  await ada.route(/^https:\/\/cdn\.example\.com\//, route => route.fulfill({ status: 404, body: "" }));
  const page = await ada.newPage();
  const errors = watchErrors(page);
  const fetchedElsewhere: string[] = [];
  page.on("request", request => {
    if (request.url().includes("cdn.example.com")) fetchedElsewhere.push(request.url());
  });
  await page.goto(`/c/${group.id}`);
  const note = page.locator("article[data-message-code]", { hasText: elsewhere });
  await expect(note.getByRole("button", { name: /^Play Bo's voice message/ })).toBeVisible();
  await expect(note).toContainText("Hello from somewhere else.");
  expect(fetchedElsewhere, "nothing is fetched from elsewhere before it plays").toEqual([]);

  // ↑ in the empty field edits Ada's last message; meanwhile ⌘⇧M says why it waits, and the microphone dictates.
  const field = page.getByRole("textbox", { name: /^Message / });
  await field.click();
  await field.press("ArrowUp");
  await expect(page.getByRole("textbox", { name: "Edit your message" })).toHaveValue(mine);
  await page.keyboard.press(`${MOD}+Shift+M`);
  await expect(page.getByText("A voice message waits for the edit")).toBeVisible();
  await expect(page.getByRole("group", { name: "Recording a voice message" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Dictate into your edit" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("button", { name: "Record a voice message" })).toBeVisible();

  expect(errors).toEqual([]);
  await ada.close();
});

/* Listening to Silicons --------------------------------------------------------------------------------------------- */

test("Listen plays a Silicon's words; a spoken message prepares, then shows its waveform; Listen mode speaks what arrives", async ({ browser }) => {
  // A group of this run's own with Scout: Listen mode hears only what this test has Scout say.
  const group = await groupWithScout("Listen check");
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  await page.goto(`/c/${group}`);
  await expect(page.getByRole("textbox", { name: /^Message / })).toBeVisible();

  // Listen on a text message (the message menu, or Space on a selected message): spoken in Scout's voice.
  const words = `Listen check ${stamp()}`;
  scoutSays(group, ["--text", `${words}, the build is green.`]);
  const text = page.locator("article[data-message-code]", { hasText: words });
  await expect(text).toBeVisible();
  const textCode = await text.getAttribute("data-message-code");
  // Where it rests once it has risen in (a message that arrives rises 6 px into place).
  const textTop = await text.evaluate(async node => {
    await Promise.all(node.getAnimations({ subtree: true }).filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => undefined)));
    return node.getBoundingClientRect().top;
  });
  await text.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Listen" }).click();
  await expectPlaying(page, `|${textCode}`);
  // Listening moves nothing (its progress is drawn inside the bubble), and the thread says it to screen readers.
  expect(Math.abs((await text.evaluate(node => node.getBoundingClientRect().top)) - textTop), "the message stays where it was").toBeLessThanOrEqual(1);
  await expect(page.getByRole("main").getByRole("status").filter({ hasText: /^Playing$/ })).toHaveCount(1);

  // A voice that cannot say the words: the Carbon reads what happened and what to do, never the service's wording for
  // Silicons ("choose another voice from GET /api/v1/voices"). This page's speech requests only.
  const refusing = (url: URL) => url.pathname.includes(`/conversations/${encodeURIComponent(group)}/messages/`) && url.pathname.endsWith("/speech");
  await page.route(refusing, route => route.fulfill({ status: 422, headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "error", data: { error: { code: "speech_rejected", message: "Deepgram refused this voice or script; retrying the same script and voice will not help.", hint: "Change the script (plain words, cues in square brackets) or choose another voice from GET /api/v1/voices." } } }) }));
  const refused = `Refused words ${stamp()}`;
  scoutSays(group, ["--text", `${refused}, in a voice that cannot say them.`]);
  const refusedRow = page.locator("article[data-message-code]", { hasText: refused });
  await expect(refusedRow).toBeVisible();
  await refusedRow.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Listen" }).click();
  await expect(page.getByText("The voice could not say these words, so there is no audio for them.")).toBeVisible();
  await expect(page.getByText(/GET \/api\/v1\/voices|Deepgram/)).toHaveCount(0);
  await page.unroute(refusing);

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
  const sending = spawn("node", [HELPER, "dm", "scout", "--", "messages", "send", group, "--speak", `[warmly] Hi Ada, ${spoken}. ${long}.`, "--dangerously-send-long-message", "--wait"], { stdio: "ignore" });
  const sent = new Promise(resolve => sending.once("exit", resolve));
  const speech = page.locator("article[data-message-code]", { hasText: spoken });
  await expect(speech).toBeVisible({ timeout: 30_000 });
  await expect(speech.getByRole("button", { name: /^Play Scout's spoken message, \d+:\d\d/ })).toBeVisible({ timeout: 30_000 });
  await expect(speech.getByRole("slider", { name: /^Position in Scout's spoken message/ }).locator("svg")).toBeVisible();
  await expect(speech).not.toContainText("Preparing audio…");
  expect(await page.evaluate(marker => ((window as unknown as { __preparing?: string[] }).__preparing ?? []).some(text => text.includes(marker)), spoken), "it said Preparing audio… while the service rendered it").toBe(true);
  await sent;
  await expectFocusSeen(page, "a spoken message's Play", speech.getByRole("button", { name: /^Play Scout's spoken message/ }));
  await expectFocusSeen(page, "a spoken message's waveform", speech.getByRole("slider", { name: /^Position in Scout's spoken message/ }));
  // Seeking by keyboard on its waveform once it plays (the speed that shows meanwhile does not grow the bubble).
  const bubbleHeight = () => speech.evaluate(node => node.querySelector("[data-sq='surface']")!.getBoundingClientRect().height);
  const resting = await bubbleHeight();
  await speech.getByRole("button", { name: /^Play Scout's spoken message/ }).click();
  const speechCode = await speech.getAttribute("data-message-code");
  await expectPlaying(page, `|${speechCode}`);
  expect(Math.abs((await bubbleHeight()) - resting), "playing does not grow the bubble").toBeLessThanOrEqual(1);
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
  scoutSays(group, ["--text", `${arriving}, ${Array.from({ length: 10 }, (_, index) => `point ${index + 1} still holds`).join(", ")}.`, "--dangerously-send-long-message"]);
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
  scoutSays(group, ["--speak", `[warmly] ${said}, all done.`]);
  const spokenNext = page.locator("article[data-message-code]", { hasText: said });
  await expect(spokenNext).toBeVisible();
  await expectPlaying(page, `|${await spokenNext.getAttribute("data-message-code")}`);
  await page.getByRole("button", { name: "Stop speaking" }).click();

  // A second tab on this conversation with Listen mode on: one tab speaks, the one where it was turned on last. The
  // other says so, and Listen here brings it back.
  const other = await ada.newPage();
  const otherErrors = watchErrors(other);
  await other.goto(`/c/${group}`);
  await other.getByRole("button", { name: "Listen mode" }).click();
  await expect(page.getByRole("status").filter({ hasText: "On in another tab" })).toBeVisible();
  const twice = `Two tabs ${stamp()}`;
  scoutSays(group, ["--text", `${twice}, ${Array.from({ length: 8 }, (_, index) => `point ${index + 1} holds`).join(", ")}.`, "--dangerously-send-long-message"]);
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
  scoutSays(group, ["--text", `${doomed}, ${Array.from({ length: 12 }, (_, index) => `sentence ${index + 1} keeps going`).join(", ")}.`, "--dangerously-send-long-message"]);
  const doomedRow = page.locator("article[data-message-code]", { hasText: doomed });
  await expect(doomedRow).toBeVisible();
  const doomedCode = (await doomedRow.getAttribute("data-message-code"))!;
  await expectPlaying(page, `|${doomedCode}`);
  helper("dm", "scout", "--", "messages", "delete", group, doomedCode);
  await expect.poll(async () => (await player(page))?.track ?? null, { timeout: 15_000, message: "the deleted message stops playing" }).toBeNull();

  // ⌘⇧L turns it off again.
  await page.keyboard.press(`${MOD}+Shift+L`);
  await expect(toggle).toHaveAttribute("aria-pressed", "false");

  // What was typed is gone from the stack too.
  await dmApi("ada", "DELETE", `/api/v1/conversations/${encodeURIComponent(group)}/draft`).catch(() => undefined);
  expect(errors).toEqual([]);
  await ada.close();
});

test("Listen mode is offered only where a Silicon takes part: elsewhere the palette leaves it out and ⌘⇧L says why", async ({ browser }) => {
  const carbons = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name: `Carbons only ${stamp()}`, member_ids: ["c:dmc-bo"] } });
  const withScout = await groupWithScout("Listen offered");
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  const paletteOffers = async () => {
    await page.keyboard.press(`${MOD}+k`);
    await page.keyboard.type("Listen mode");
    const options = page.getByRole("option", { name: /^Listen mode/ });
    await page.waitForTimeout(300);
    const count = await options.count();
    await page.keyboard.press("Escape");
    return count;
  };
  await page.goto(`/c/${carbons.id}`);
  await expect(page.getByRole("textbox", { name: /^Message / })).toBeVisible();
  await expect(page.getByRole("button", { name: "Show details" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Listen mode" })).toHaveCount(0);
  await page.keyboard.press(`${MOD}+Shift+L`);
  await expect(page.getByText("Nothing for Listen mode here")).toBeVisible();
  expect(await paletteOffers(), "the palette leaves Listen mode out where nothing could be spoken").toBe(0);
  await page.goto(`/c/${withScout}`);
  await expect(page.getByRole("button", { name: "Listen mode" })).toBeVisible();
  expect(await paletteOffers()).toBe(1);
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
  // The default voice's card says so after its name, as a word of its own ("Mark Default", never "MarkDefault").
  await expect(voices.getByRole("radio", { name: /Default$/ })).toHaveAccessibleName(/\S Default$/);
  await expectFocusSeen(page, "a voice's sample", page.getByRole("button", { name: "Play Hope's sample" }).first());

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
  // Who chose it and when, on one line: where the time does not fit (a medium window) it gives way whole, never cut.
  await page.setViewportSize({ width: 780, height: 900 });
  const chosenLine = page.getByRole("region", { name: "Scout's voice" }).locator("[title*='Chosen by you']");
  await expect.poll(() => chosenLine.evaluate(line => {
    const [main, when] = [line.firstElementChild as HTMLElement, line.lastElementChild as HTMLElement];
    const shown = when !== main && when.getBoundingClientRect().top - line.getBoundingClientRect().top < line.clientHeight - 1;
    return { mainCut: main.scrollWidth > main.clientWidth + 1, timeCut: shown && when.getBoundingClientRect().right > line.getBoundingClientRect().right + 1 };
  })).toEqual({ mainCut: false, timeCut: false });
  await page.setViewportSize({ width: 1280, height: 720 });

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

/* A caption, a recording kept across a reload, a phone ----------------------------------------------------------------- */

test("a spoken message's written caption is the message; its spoken words are the transcript under the player", async ({ browser }) => {
  const group = await groupWithScout("Caption check");
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  await page.goto(`/c/${group}`);
  await expect(page.getByRole("textbox", { name: /^Message / })).toBeVisible();
  const caption = `Build time ${stamp()}: 1.8 s`;
  scoutSays(group, ["--speak", "[warmly] Down to one point eight seconds, from two point four.", "--text", caption]);
  const spoken = page.locator("article[data-message-code]", { hasText: caption });
  await expect(spoken).toBeVisible({ timeout: 30_000 });
  const transcript = spoken.getByRole("button", { name: /^Transcript: Down to one point eight seconds/ });
  await expect(transcript).toHaveAttribute("aria-expanded", "false");
  const order = await spoken.evaluate((article, words) => {
    const captionNode = [...article.querySelectorAll("p")].find(node => node.textContent?.includes(words));
    const player = article.querySelector("[data-voice-ui]");
    const said = [...article.querySelectorAll("button")].find(node => node.textContent?.includes("Down to one point eight"));
    if (!captionNode || !player || !said) return "missing";
    const follows = (a: Node, b: Node) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
    return follows(captionNode, player) && follows(player, said) ? "caption, player, spoken words" : "out of order";
  }, caption);
  expect(order).toBe("caption, player, spoken words");
  await transcript.click();
  await expect(spoken.getByRole("button", { name: /^Hide the transcript: Down to one point eight seconds/ })).toHaveAttribute("aria-expanded", "true");
  // Its name, as the log reads it, is its caption.
  await expect(spoken).toHaveAttribute("aria-label", /^Message from Scout .*: Build time .*: 1\.8 s$/);
  expect(errors).toEqual([]);
  await ada.close();
});

test("a voice message cut off by a reload is kept on disk, offered when its conversation opens again, and sent from there", async ({ browser }) => {
  const group = await dmApi<{ id: string }>("ada", "POST", "/api/v1/groups", { type: "create_group", data: { name: `Voice kept ${stamp()}`, member_ids: ["c:dmc-bo"] } });
  const ada = await signedIn(browser, "ada");
  const page = await ada.newPage();
  const errors = watchErrors(page);
  await page.goto(`/c/${group.id}`);
  const field = page.getByRole("textbox", { name: /^Message / });
  await expect(field).toBeVisible();
  await field.click();
  await field.press(`${MOD}+Shift+M`);
  const recording = page.getByRole("group", { name: "Recording a voice message" });
  await expect(recording.getByRole("timer")).toHaveText(/0:(0[7-9]|1\d)/, { timeout: 20_000 });
  // Written to this browser's own disk as it records, a few seconds at a time, with a record of what it is.
  type Kept = { mode: string; bytes: number; store: string };
  const kept = () => page.evaluate(() => Object.values(JSON.parse(localStorage.getItem("dm:recordings:v1") ?? "{}") as Record<string, unknown>)) as Promise<Kept[]>;
  await expect.poll(async () => (await kept()).find(take => take.mode === "note")?.bytes ?? 0).toBeGreaterThan(0);
  expect((await kept()).map(take => take.store)).toEqual(["opfs"]);

  // The page reloads mid-recording: what was written is offered, with how long it is.
  await page.reload();
  const offer = page.getByRole("group", { name: "A voice message that was not sent" });
  await expect(offer).toBeVisible({ timeout: 15_000 });
  await expect(offer).toContainText(/0:0[4-9] long, recorded today at \d\d:\d\d/);
  await expect(offer.getByRole("button", { name: "Send" })).toBeFocused();
  await page.keyboard.press("Enter");
  type Stored = { items: Array<{ voice: { duration_milliseconds: number } | null }> };
  await expect.poll(async () => (await dmApi<Stored>("ada", "GET", `/api/v1/conversations/${encodeURIComponent(group.id)}/messages?limit=5`)).items.find(item => item.voice)?.voice?.duration_milliseconds ?? 0, { timeout: 30_000, message: "it went as a voice message" }).toBeGreaterThan(4000);
  await expect(page.locator("article[data-message-code]").getByRole("button", { name: /^Play your voice message/ })).toBeVisible({ timeout: 30_000 });
  // Sent: nothing is left on disk.
  await expect.poll(kept).toEqual([]);
  expect(errors).toEqual([]);
  await ada.close();
});

test("on a phone, Change opens Scout's voices as a sheet; a playing spoken message keeps its voice's name", async ({ browser }) => {
  const ada = await signedIn(browser, "ada", { viewport: { width: 390, height: 844 } });
  const page = await ada.newPage();
  const errors = watchErrors(page);
  // Ada's own conversation with Scout: its details (a bottom sheet on a phone) show Scout's voice.
  await page.goto("/c/c:dmc-ada::si:dmc-scout");
  await page.getByRole("button", { name: "Show details" }).click();
  await page.getByRole("button", { name: "Change Scout's voice" }).click();
  const sheet = page.getByRole("dialog", { name: "Scout's voice" });
  await expect(sheet).toBeVisible();
  await expect(sheet.getByRole("radiogroup", { name: "Scout's voice" })).toBeVisible();
  const box = await sheet.boundingBox();
  expect(box && box.x >= 0 && box.x + box.width <= 390, "the sheet stays on the screen").toBe(true);
  await page.keyboard.press("Escape");
  await expect(sheet).toHaveCount(0);

  // A spoken message plays: the voice's name stays beside the time; the speed is one button that steps.
  const group = await groupWithScout("Voice phone");
  await page.goto(`/c/${group}`);
  await expect(page.getByRole("textbox", { name: /^Message / })).toBeVisible();
  const words = `Phone check ${stamp()}`;
  scoutSays(group, ["--speak", `[cheerfully] ${words} is green, and the voice keeps its name.`]);
  const spoken = page.locator("article[data-message-code]", { hasText: words });
  const play = spoken.getByRole("button", { name: /^Play Scout's spoken message, \d+:\d\d/ });
  await expect(play).toBeVisible({ timeout: 30_000 });
  await play.click();
  await expectPlaying(page, `|${await spoken.getAttribute("data-message-code")}`);
  await expect(spoken.locator("[title^='Spoken in the voice']")).toBeVisible();
  const speed = spoken.getByRole("button", { name: /^1× playback speed\. Press for 1\.5×/ });
  await expect(speed).toBeVisible();
  await speed.click();
  await expect.poll(() => page.evaluate(() => document.querySelector<HTMLAudioElement>("audio[data-dm-player]")?.playbackRate)).toBe(1.5);
  await spoken.getByRole("button", { name: /^1\.5× playback speed/ }).click();
  await spoken.getByRole("button", { name: /^2× playback speed/ }).click();
  await expect.poll(() => page.evaluate(() => document.querySelector<HTMLAudioElement>("audio[data-dm-player]")?.playbackRate)).toBe(1);
  await expect(spoken.locator("[title^='Spoken in the voice']")).toBeVisible();
  expect(errors).toEqual([]);
  await ada.close();
});
