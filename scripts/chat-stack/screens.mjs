#!/usr/bin/env node
// The screenshot harness: every surface of the DM website, signed in as the demo's Carbon (Mira Patel, made by
// scripts/chat-stack/seed-demo.mjs), at 1440 × 900 and 390 × 844, into one folder with an index.md. It is what a
// design review looks at: DM as a Carbon sees it, not test data.
//
//   node scripts/chat-stack/screens.mjs --base http://127.0.0.1:4530 --out <folder>
//     --only inbox,sol-top     only these surfaces (--list prints them all)
//     --skip listen-mode       all but these
//     --widths 1440,390        the widths (heights 900 and 844)
//     --timezone auto          the browser's time zone; auto picks one where the demo's messages read as this morning
//
// Files are <surface>-<width>.png. The browser is Chromium from the website's Playwright with a fake microphone (a
// voiced tone), signed in with a sealed session cookie (identities.mjs cookie mira; a new sign-in per width).
//
// It leaves the demo as it found it: it never marks anything read (the website's receipts are held in its own
// browser), clears what it typed, discards the send it fails on purpose, cancels recordings and uploads. One thing
// stays: Listen mode needs a message to arrive, so Wren sends one line per run (every width listens at once); at the
// end everything older in Wren's conversation is marked read, so the inbox keeps exactly one unread message there.
// The other accounts look online (presence leases) while it runs, and Bo types while his conversation is captured.
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { DEV, HERE, StackError, describe, readJson, sleep, stackFile, writeJsonPrivate } from './lib/common.mjs';
import { appTokens, dmCall, findIdentity, loadPool, withDemo } from './lib/pool.mjs';
import { playwright } from './lib/demo-assets.mjs';

const argv = process.argv.slice(2);
const option = (name, fallback) => {
  const index = argv.indexOf(`--${name}`);
  return index >= 0 && argv[index + 1] && !argv[index + 1].startsWith('--') ? argv[index + 1] : fallback;
};
const flag = (name) => argv.includes(`--${name}`);
const say = (text) => process.stderr.write(`screens: ${text}\n`);
const MOD = process.platform === 'darwin' ? 'Meta' : 'Control';
const HEIGHTS = { 1440: 900, 390: 844 };

const stack = stackFile();
const base = option('base', stack.website?.e2e_url ?? 'http://127.0.0.1:4530').replace(/\/+$/, '');
const out = option('out') ? resolve(option('out')) : null;
const widths = option('widths', '1440,390').split(',').map(Number).filter(Boolean);

// ------------------------------------------------------------------------------------------------ the demo

const demo = readJson(join(DEV, 'demo.json'), null);
let pool = null;
const who = (name) => findIdentity(pool, name);
const address = (key) => demo?.conversations?.[key]?.address;
const code = (key, step) => demo?.conversations?.[key]?.steps?.[step]?.code;

const tokens = new Map();
async function tokenOf(name) {
  if (!tokens.has(name)) tokens.set(name, (await appTokens(who(name), 'dm', stack)).access_token);
  return tokens.get(name);
}
async function dm(name, method, path, body) {
  const answer = await dmCall(await tokenOf(name), method, path, body, { stack });
  if (answer.status >= 300) throw new StackError(`${method} ${path} as ${who(name).id} failed: ${describe(answer)}`);
  return answer.json?.data ?? null;
}

/** A Silicon says something through its CLI (as Silicons do); answers the accepted message. */
function cliSend(name, conversation, text) {
  const done = spawnSync(process.execPath, [join(HERE, 'identities.mjs'), 'dm', name, '--', '--json', 'messages', 'send', conversation, '--text', text, '--wait'], { encoding: 'utf8', timeout: 60_000 });
  const parsed = (() => {
    try {
      return JSON.parse(done.stdout.trim().split('\n').pop());
    } catch {
      return null;
    }
  })();
  if (parsed?.response?.state !== 'completed') throw new StackError(`${who(name).id} could not send through the CLI: ${(done.stderr || done.stdout).trim().slice(-300)}`);
  return parsed.response.result;
}

/** Who looks online while the harness runs (a presence lease each, renewed); Bo's activity is set per surface. */
const presence = { timer: null, activity: { bo: null } };
async function lease(name, activity = null) {
  await dmCall(await tokenOf(name), 'PUT', '/api/v1/presence/devices/demo-screens', { type: 'renew_presence', data: { activity } }, { stack });
}
async function startPresence() {
  const renew = async () => {
    for (const name of ['sol', 'wren', 'bo']) await lease(name, presence.activity[name] ?? null).catch(() => undefined);
  };
  await renew();
  presence.timer = setInterval(() => void renew(), 15_000);
}
async function stopPresence() {
  clearInterval(presence.timer);
  for (const name of ['sol', 'wren', 'bo']) await dmCall(await tokenOf(name), 'DELETE', '/api/v1/presence/devices/demo-screens', undefined, { stack }).catch(() => undefined);
}

/** A time zone where the demo's first message reads as this morning (the seed cannot backdate messages). */
function pickTimezone() {
  const wanted = option('timezone', 'auto');
  if (wanted !== 'auto') return wanted;
  const first = Date.parse(demo?.conversations?.sol?.steps?.['sol-morning']?.created_at ?? '') || Date.now();
  const zones = ['Pacific/Honolulu', 'America/Los_Angeles', 'America/Denver', 'America/Chicago', 'America/New_York', 'America/Sao_Paulo', 'Atlantic/Azores',
    'Europe/London', 'Europe/Berlin', 'Europe/Athens', 'Asia/Dubai', 'Asia/Karachi', 'Asia/Kolkata', 'Asia/Dhaka', 'Asia/Bangkok', 'Asia/Singapore',
    'Asia/Tokyo', 'Australia/Brisbane', 'Australia/Sydney', 'Pacific/Noumea', 'Pacific/Auckland'];
  const minutesOf = (zone) => {
    const parts = new Intl.DateTimeFormat('en-GB', { timeZone: zone, hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).formatToParts(new Date(first));
    return Number(parts.find((p) => p.type === 'hour').value) * 60 + Number(parts.find((p) => p.type === 'minute').value);
  };
  const target = 9 * 60 + 20;
  return zones.map((zone) => ({ zone, off: Math.abs(minutesOf(zone) - target) })).sort((a, b) => a.off - b.off)[0].zone;
}

/** Six seconds of a voiced tone in syllables, Chrome's fake microphone (silence would transcribe to nothing). */
function fakeMicrophone() {
  const path = join(DEV, 'demo', 'fake-microphone.wav');
  if (existsSync(path)) return path;
  const rate = 48_000;
  const samples = rate * 6;
  const bytes = Buffer.alloc(44 + samples * 2);
  bytes.write('RIFF', 0);
  bytes.writeUInt32LE(36 + samples * 2, 4);
  bytes.write('WAVEfmt ', 8);
  bytes.writeUInt32LE(16, 16);
  bytes.writeUInt16LE(1, 20);
  bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(rate, 24);
  bytes.writeUInt32LE(rate * 2, 28);
  bytes.writeUInt16LE(2, 32);
  bytes.writeUInt16LE(16, 34);
  bytes.write('data', 36);
  bytes.writeUInt32LE(samples * 2, 40);
  for (let index = 0; index < samples; index++) {
    const t = index / rate;
    const syllable = Math.floor(t * 4) % 5 === 4 ? 0.05 : 1;
    const pitch = 140 + 25 * Math.sin(2 * Math.PI * 0.7 * t);
    const value = (0.55 * Math.sin(2 * Math.PI * pitch * t) + 0.25 * Math.sin(4 * Math.PI * pitch * t)) * (0.5 + 0.5 * Math.sin(5 * Math.PI * t)) * syllable * 0.8;
    bytes.writeInt16LE(Math.round(Math.max(-1, Math.min(1, value)) * 32767), 44 + index * 2);
  }
  mkdirSync(join(DEV, 'demo'), { recursive: true, mode: 0o700 });
  writeFileSync(path, bytes);
  return path;
}

// ------------------------------------------------------------------------------------------------ page helpers

/** Waits until what is on screen is still: fonts, the pictures in view, and no finite animation running. */
async function settle(page, extra = 250) {
  await page.evaluate(() => document.fonts.ready).catch(() => undefined);
  await page.waitForFunction(() => [...document.images].every((image) => {
    const box = image.getBoundingClientRect();
    const inView = box.bottom > 0 && box.top < innerHeight && box.right > 0 && box.left < innerWidth && box.width > 0;
    return !inView || image.complete;
  }), undefined, { timeout: 10_000 }).catch(() => undefined);
  await page.waitForFunction(() => document.getAnimations().every((animation) => animation.playState !== 'running' || animation.effect?.getTiming().iterations === Infinity), undefined, { timeout: 5_000 }).catch(() => undefined);
  await page.waitForTimeout(extra);
}

/** Opens a page of the website and waits for the app to be React's (hydrated), not only the server's HTML. */
async function open(page, path) {
  await page.goto(`${base}${path}`, { waitUntil: 'domcontentloaded' });
  await page.locator('#main').first().waitFor({ timeout: 30_000 });
  await page.waitForFunction(() => {
    const main = document.getElementById('main');
    return !!main && Object.keys(main).some((key) => key.startsWith('__react'));
  }, undefined, { timeout: 30_000 }).catch(() => undefined);
}

/** Opens a conversation and waits for its messages (and its composer, unless it is a read-only view). */
async function conversation(page, key, query = '') {
  const where = address(key);
  if (!where) throw new StackError(`The demo has no ${key} conversation.`, 'Run node scripts/chat-stack/seed-demo.mjs first.');
  await open(page, `/c/${where}${query}`);
  await page.locator('article[data-message-code]').first().waitFor({ timeout: 30_000 });
  if (!query) {
    await page.getByRole('textbox', { name: /^Message / }).waitFor({ timeout: 30_000 });
    await page.waitForFunction(() => {
      const field = document.querySelector('[data-composer-field]');
      return !!field && Object.keys(field).some((key) => key.startsWith('__reactProps'));
    }, undefined, { timeout: 30_000 });
  }
  await settle(page, 400);
}

/** Scrolls the open thread to its top or its bottom. */
async function scrollThread(page, where) {
  await page.evaluate((to) => {
    const log = document.querySelector('[role="log"]');
    let node = log;
    while (node && node !== document.body) {
      const style = getComputedStyle(node);
      if (/(auto|scroll)/.test(style.overflowY) && node.scrollHeight > node.clientHeight + 1) break;
      node = node.parentElement;
    }
    if (node && node !== document.body) node.scrollTop = to === 'top' ? 0 : node.scrollHeight;
  }, where);
  await settle(page, 500);
}

/** Puts the keyboard nowhere in particular, so single-key shortcuts (N, ?, G then I) are not typed into a field. */
async function blur(page) {
  await page.evaluate(() => (document.activeElement instanceof HTMLElement ? document.activeElement.blur() : undefined));
}

async function detailsOpen(page) {
  const panel = page.locator('[data-details-panel]');
  if (!(await panel.isVisible().catch(() => false))) await page.getByRole('button', { name: 'Show details' }).first().click();
  await panel.waitFor({ state: 'visible', timeout: 15_000 });
  await settle(page, 400);
  return panel;
}

async function detailsClosed(page) {
  const hide = page.getByRole('button', { name: 'Hide details' }).first();
  if (await hide.isVisible().catch(() => false)) await hide.click();
  else await page.keyboard.press('Escape');
  await page.locator('[data-details-panel]').waitFor({ state: 'hidden', timeout: 10_000 }).catch(() => undefined);
}

const row = (page, text) => page.locator('article[data-message-code]', { hasText: text }).first();
const field = (page) => page.getByRole('textbox', { name: /^Message / });

/** Clears the composer and the draft the website saved for it at the service. */
async function clearComposer(page, key) {
  await field(page).fill('').catch(() => undefined);
  await page.waitForTimeout(1800);
  await dmCall(await tokenOf('mira'), 'DELETE', `/api/v1/conversations/${encodeURIComponent(address(key))}/draft`, undefined, { stack }).catch(() => undefined);
}

/** What the shared audio element is doing (the website's one player). */
const player = (page) => page.evaluate(() => {
  const audio = document.querySelector('audio[data-dm-player]');
  return audio ? { track: audio.dataset.track ?? null, time: audio.currentTime, paused: audio.paused } : null;
});

// ------------------------------------------------------------------------------------------------ the surfaces

/**
 * Every surface, in the order they are taken. Each runs at every width with the same signed-in page (a fresh page for
 * the signed-out ones); the ones that change something come last, so the inbox and the threads are taken as seeded.
 */
const SURFACES = [
  { name: 'landing', what: 'The public landing page, signed out', signedOut: true, fullPage: true, run: async ({ page }) => {
    await page.goto(`${base}/`, { waitUntil: 'load' });
    await settle(page, 500);
  } },
  { name: 'sign-in', what: 'Sign in, signed out', signedOut: true, run: async ({ page }) => {
    await page.goto(`${base}/sign-in`, { waitUntil: 'load' });
    await settle(page, 500);
  } },
  { name: 'inbox', what: 'The inbox: four conversations, unread badges (Launch week 2, Wren 1), an invitation, Mira\'s account at the bottom', run: async ({ page }) => {
    await open(page, '/c');
    await page.locator('a[data-row]:visible').nth(3).waitFor({ timeout: 30_000 });
    await settle(page, 600);
  } },
  { name: 'sol-top', what: 'Mira and Sol, top: the morning summary (a spoken message with a caption), the pricing PDF, a code block', run: async ({ page }) => {
    await conversation(page, 'sol');
    await scrollThread(page, 'top');
  } },
  { name: 'sol-bottom', what: 'Mira and Sol, bottom: a reply that quotes and was edited, Mira\'s voice note with its transcript, a short spoken message, Read and Delivered', run: async ({ page }) => {
    await conversation(page, 'sol');
    await scrollThread(page, 'bottom');
  } },
  { name: 'wren-top', what: 'Mira and Wren, top: the nightly build, Mira\'s question, Wren\'s spoken answer', run: async ({ page }) => {
    await conversation(page, 'wren');
    await scrollThread(page, 'top');
  } },
  { name: 'wren-bottom', what: 'Mira and Wren, bottom: Wren\'s latest updates, the newest one unread under "New messages"', run: async ({ page }) => {
    await conversation(page, 'wren');
    await scrollThread(page, 'bottom');
  } },
  { name: 'group-top', what: 'Launch week, top: Bo\'s two hero mock-ups, Sol\'s reply, Wren\'s chart, Mira\'s reply', run: async ({ page }) => {
    await conversation(page, 'group');
    await scrollThread(page, 'top');
  } },
  { name: 'group-middle', what: 'Launch week, middle: Wren\'s chart, Mira\'s reply that quotes it, the whiteboard photo', run: async ({ page }) => {
    await conversation(page, 'group');
    await row(page, 'Signups this week against last week').evaluate((node) => node.scrollIntoView({ block: 'start' }));
    await settle(page, 500);
  } },
  { name: 'group-bottom', what: 'Launch week, bottom: the whiteboard photo, Sol\'s bundled checklist, "New messages", a message Wren routed to Sol, Bo\'s GIF', run: async ({ page }) => {
    await conversation(page, 'group');
    await scrollThread(page, 'bottom');
  } },
  { name: 'bo-typing', what: 'Mira and Bo, a Carbon conversation, while Bo types (header and the end of the thread)', run: async ({ page }) => {
    presence.activity.bo = 'typing';
    await lease('bo', 'typing');
    const keep = setInterval(() => void lease('bo', 'typing').catch(() => undefined), 2_000);
    try {
      await conversation(page, 'bo');
      await scrollThread(page, 'bottom');
      await page.getByText('Bo is typing…').first().waitFor({ timeout: 25_000 }).catch(() => say('Bo\'s typing did not show within 25 s (presence is read every 15 s)'));
      await settle(page, 300);
    } finally {
      clearInterval(keep);
    }
  }, after: async () => {
    presence.activity.bo = null;
    await lease('bo', null).catch(() => undefined);
  } },
  { name: 'custodian-view', what: 'Viewing as Sol: Sol and Wren\'s own conversation, read only, with the intents each message was routed to', run: async ({ page }) => {
    await conversation(page, 'silicons', `?for=${encodeURIComponent(who('sol').id)}`);
  } },
  { name: 'speech-playing', what: 'The morning summary playing (progress on its waveform, speed)', run: async ({ page }) => {
    await conversation(page, 'sol');
    const spoken = row(page, 'Your morning in one minute');
    await spoken.scrollIntoViewIfNeeded();
    await spoken.getByRole('button', { name: /^Play Sol's spoken message/ }).click();
    await page.waitForFunction(() => (document.querySelector('audio[data-dm-player]')?.currentTime ?? 0) > 2.2, undefined, { timeout: 30_000 });
    await settle(page, 100);
  }, after: async ({ page }) => {
    const playing = await player(page).catch(() => null);
    if (playing && !playing.paused) await page.getByRole('button', { name: /^Pause Sol's spoken message/ }).first().click().catch(() => undefined);
  } },
  { name: 'message-menu', what: 'A Silicon\'s message and its menu (right click): reply, copy, listen, details', run: async ({ page }) => {
    await conversation(page, 'sol');
    // A plain message (a link or a picture keeps the browser's own menu), clicked at the foot of its words so the
    // menu opens under them instead of over them.
    const message = row(page, 'Noted. I');
    await message.scrollIntoViewIfNeeded();
    const box = await message.getByText(/^Noted\. I/).first().boundingBox();
    await page.mouse.click(box.x + Math.min(80, box.width / 2), box.y + box.height - 3, { button: 'right' });
    await page.getByRole('menu', { name: 'Message actions' }).waitFor();
    await settle(page);
  } },
  { name: 'message-details', what: 'Message details: the message as JSON and the CLI commands for it', run: async ({ page }) => {
    await conversation(page, 'sol');
    const message = row(page, 'pricing v3, with Bo');
    await message.scrollIntoViewIfNeeded();
    await message.click({ button: 'right', position: { x: 40, y: 18 } });
    await page.getByRole('menuitem', { name: 'Message details' }).click();
    await page.getByRole('dialog', { name: /^Message / }).waitFor();
    await settle(page, 400);
  } },
  { name: 'edit-history', what: 'Edited: the earlier version of Mira\'s reply', run: async ({ page }) => {
    await conversation(page, 'sol');
    const edited = row(page, 'Page 2 looks right');
    await edited.scrollIntoViewIfNeeded();
    await edited.getByRole('button', { name: /^Edited/ }).click();
    await page.getByRole('dialog').filter({ hasText: /rigth|Before the last edit/ }).first().waitFor();
    await settle(page);
  } },
  { name: 'bundle-open', what: 'Sol\'s bundle opened: the three messages it stands for', run: async ({ page }) => {
    await conversation(page, 'group');
    const bundle = row(page, 'Launch checklist');
    await bundle.scrollIntoViewIfNeeded();
    await bundle.getByRole('button', { name: /messages bundled/ }).click();
    await bundle.getByText('Press kit: done.').waitFor();
    await bundle.scrollIntoViewIfNeeded();
    await settle(page, 400);
  } },
  { name: 'lightbox', what: 'A photo in the lightbox (Bo\'s second hero mock-up)', run: async ({ page }) => {
    await conversation(page, 'group');
    const heroes = row(page, 'hero options');
    await heroes.scrollIntoViewIfNeeded();
    await heroes.getByRole('button', { name: /^Open photo 2 of 2/ }).click();
    const box = page.getByRole('dialog').first();
    await box.waitFor();
    await page.waitForFunction(() => [...document.querySelectorAll('[role=dialog] img')].every((image) => image.complete && image.naturalWidth > 0), undefined, { timeout: 15_000 });
    await settle(page, 400);
  } },
  { name: 'details-carbon', what: 'The details panel of a Carbon (Bo)', run: async ({ page }) => {
    await conversation(page, 'bo');
    await detailsOpen(page);
  }, after: ({ page }) => detailsClosed(page) },
  { name: 'details-silicon', what: 'The details panel of a Silicon (Sol): custodian, voice with Preview and Change, reach', run: async ({ page }) => {
    await conversation(page, 'sol');
    await detailsOpen(page);
  }, after: ({ page }) => detailsClosed(page) },
  { name: 'details-silicon-voice', what: 'Sol\'s voice, Change open: the five voices as cards', run: async ({ page }) => {
    await conversation(page, 'sol');
    const panel = await detailsOpen(page);
    await panel.getByRole('button', { name: /^Change Sol.s voice/ }).click();
    await page.getByRole('radiogroup').first().waitFor();
    await settle(page, 500);
  }, after: async ({ page }) => {
    await page.keyboard.press('Escape');
    await detailsClosed(page);
  } },
  { name: 'details-group', what: 'The details panel of a group (Launch week): owners, members, address and CLI', run: async ({ page }) => {
    await conversation(page, 'group');
    await detailsOpen(page);
  }, after: ({ page }) => detailsClosed(page) },
  { name: 'details-group-rename', what: 'Renaming the group in place (not saved)', run: async ({ page }) => {
    await conversation(page, 'group');
    const panel = await detailsOpen(page);
    await panel.getByRole('button', { name: /^Group name: / }).click();
    await panel.getByRole('textbox', { name: 'Group name' }).waitFor();
    await settle(page);
  }, after: async ({ page }) => {
    await page.keyboard.press('Escape');
    await detailsClosed(page);
  } },
  { name: 'dialog-new-conversation', what: 'New conversation (N)', run: async ({ page }) => {
    await open(page, '/c');
    await page.locator('a[data-row]:visible').first().waitFor();
    await blur(page);
    await page.keyboard.press('n');
    await page.getByRole('dialog', { name: 'New conversation' }).waitFor();
    await settle(page, 400);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'dialog-new-group', what: 'New group (Shift+N)', run: async ({ page }) => {
    await open(page, '/c');
    await page.locator('a[data-row]:visible').first().waitFor();
    await blur(page);
    await page.keyboard.press('Shift+N');
    await page.getByRole('dialog', { name: 'New group' }).waitFor();
    await settle(page, 400);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'dialog-invitations', what: 'Invitations (G then I): Bo\'s invitation to Design crit', run: async ({ page }) => {
    await open(page, '/c');
    await page.locator('a[data-row]:visible').first().waitFor();
    await blur(page);
    await page.keyboard.press('g');
    await page.keyboard.press('i');
    await page.getByRole('dialog', { name: 'Invitations' }).waitFor();
    await page.getByRole('dialog', { name: 'Invitations' }).getByText('Design crit').first().waitFor();
    await settle(page, 400);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'palette', what: 'The command palette (Cmd+K) over a conversation', run: async ({ page }) => {
    await conversation(page, 'sol');
    await field(page).click();
    await page.keyboard.press(`${MOD}+K`);
    await page.getByRole('dialog', { name: 'Search conversations and actions' }).waitFor();
    await settle(page, 400);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'cheatsheet', what: 'The keyboard shortcuts (?)', run: async ({ page }) => {
    await conversation(page, 'sol');
    await blur(page);
    await page.keyboard.press('Shift+?');
    await page.getByRole('dialog', { name: 'Keyboard shortcuts' }).waitFor();
    await settle(page, 400);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'account-menu', what: 'The account menu at the bottom of the list', run: async ({ page, width }) => {
    await open(page, '/c');
    await page.locator('a[data-row]:visible').first().waitFor();
    await page.getByRole('button', { name: /^Account menu/ }).first().click();
    await page.getByRole('menu').first().waitFor();
    await settle(page);
    void width;
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'viewing-as', what: 'Viewing as: Mira switches to the conversations of a Silicon she looks after', run: async ({ page }) => {
    await open(page, '/c');
    await page.locator('a[data-row]:visible').first().waitFor();
    await page.getByRole('button', { name: /Viewing as/ }).first().click();
    await page.locator('[role=menu], [role=listbox], [role=dialog]').first().waitFor();
    await settle(page);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'search', what: 'Searching the list (/ then words): only what matches', run: async ({ page }) => {
    await open(page, '/c');
    await page.locator('a[data-row]:visible').first().waitFor();
    await blur(page);
    await page.keyboard.press('/');
    await page.keyboard.type('launch');
    await page.waitForFunction(() => document.querySelectorAll('a[data-row]').length <= 2, undefined, { timeout: 10_000 }).catch(() => undefined);
    await settle(page, 300);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'search-empty', what: 'A search that finds nothing', run: async ({ page }) => {
    await open(page, '/c');
    await page.locator('a[data-row]:visible').first().waitFor();
    await blur(page);
    await page.keyboard.press('/');
    await page.keyboard.type('quarterly taxes');
    await page.waitForFunction(() => document.querySelectorAll('a[data-row]').length === 0, undefined, { timeout: 10_000 }).catch(() => undefined);
    await settle(page, 300);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'account-card', what: 'A sender\'s card (focus or hover on the name): who it is, and Message', run: async ({ page }) => {
    await conversation(page, 'group');
    const name = row(page, 'hero options').getByRole('button', { name: 'Bo Okafor', exact: true });
    await name.scrollIntoViewIfNeeded();
    await name.focus();
    await page.getByRole('tooltip').first().waitFor({ timeout: 10_000 });
    await settle(page, 300);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'replying', what: 'Replying (a message selected with Cmd+Up and the arrows, then R): the reply bar over the field', run: async ({ page }) => {
    await conversation(page, 'sol');
    await field(page).click();
    await page.keyboard.press(`${MOD}+ArrowUp`);
    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('r');
    await page.getByText(/^Replying to /).first().waitFor({ timeout: 10_000 });
    await page.keyboard.type('Thursday works. Thanks for sorting it out');
    await settle(page, 300);
  }, after: async ({ page }) => {
    await field(page).fill('').catch(() => undefined);
    await page.keyboard.press('Escape').catch(() => undefined);
    await clearComposer(page, 'sol');
  } },
  { name: 'editing', what: 'Editing your last message (Up in the empty field): the edit bar', run: async ({ page }) => {
    await conversation(page, 'sol');
    await field(page).click();
    await page.keyboard.press('ArrowUp');
    await page.getByRole('textbox', { name: 'Edit your message' }).waitFor({ timeout: 10_000 });
    await settle(page, 300);
  }, after: async ({ page }) => {
    await page.keyboard.press('Escape').catch(() => undefined);
    await clearComposer(page, 'sol');
  } },
  { name: 'delete-confirm', what: 'Deleting your message asks in place (Cmd+Up, then Delete); nothing is deleted', run: async ({ page }) => {
    await conversation(page, 'sol');
    await field(page).click();
    await page.keyboard.press(`${MOD}+ArrowUp`);
    await page.keyboard.press('Delete');
    await page.getByText('Delete for everyone?').first().waitFor({ timeout: 10_000 });
    await settle(page, 300);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  ...['voice', 'listening', 'sending', 'keyboard', 'notifications', 'reach', 'account', 'report'].map((section) => ({
    name: `settings-${section}`,
    what: `Settings, ${section === 'reach' ? 'Your Silicons\' reach' : section === 'report' ? 'Report a problem' : section[0].toUpperCase() + section.slice(1)}${section === 'voice' ? ' (your Silicons\' voices as radio cards, and yours)' : ''}`,
    run: async ({ page }) => {
      await open(page, `/settings#${section}`);
      await page.locator(`#${section}`).first().waitFor({ timeout: 30_000 });
      // The column grows while its sections load their data; wait until it stops, then land on the section again.
      let last = -1;
      for (let i = 0; i < 20; i++) {
        const height = await page.evaluate(() => document.getElementById('settings-scroll')?.scrollHeight ?? document.body.scrollHeight);
        if (height === last) break;
        last = height;
        await page.waitForTimeout(300);
      }
      await page.evaluate((id) => {
        window.location.hash = '';
        window.location.hash = id;
      }, section);
      await settle(page, 700);
    },
  })),
  { name: 'plus-menu', what: 'The composer\'s + menu: Upload a file, Add a link, GIF', run: async ({ page }) => {
    await conversation(page, 'sol');
    await page.getByRole('button', { name: 'Add a file, a link or a GIF' }).click();
    await page.getByRole('menu').first().waitFor();
    await settle(page);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'gif-picker', what: 'The GIF picker (trending)', run: async ({ page }) => {
    await conversation(page, 'sol');
    await page.getByRole('button', { name: 'Add a file, a link or a GIF' }).click();
    await page.getByRole('menu').getByRole('menuitem', { name: 'GIF' }).click();
    const picker = page.getByRole('dialog', { name: 'Choose a GIF' });
    await picker.waitFor();
    await picker.getByRole('button').nth(2).waitFor({ timeout: 15_000 });
    await page.waitForFunction(() => [...document.querySelectorAll('[role=dialog] img')].filter((image) => image.complete && image.naturalWidth > 0).length >= 3, undefined, { timeout: 20_000 }).catch(() => undefined);
    await settle(page, 600);
  }, after: ({ page }) => page.keyboard.press('Escape') },
  { name: 'tray-uploading', what: 'A file uploading in the tray (held on its way), with words typed', run: async ({ page, held }) => {
    await conversation(page, 'sol');
    await page.route((url) => url.pathname === '/api/uploads' && url.searchParams.has('name'), (route) => held.push(route));
    const { readFileSync } = await import('node:fs');
    const { ASSETS_DIR } = await import('./lib/demo-assets.mjs');
    const { readdirSync } = await import('node:fs');
    const pdf = readdirSync(ASSETS_DIR).find((name) => name.endsWith('.pdf'));
    await page.locator('input[data-composer-file]').setInputFiles({ name: 'Launch-day support replies.pdf', mimeType: 'application/pdf', buffer: pdf ? readFileSync(join(ASSETS_DIR, pdf)) : Buffer.from('%PDF-1.4\n%%EOF\n') });
    await page.getByRole('list', { name: 'Going with this message' }).waitFor();
    await field(page).fill('The support replies for Thursday, for a last look');
    await page.waitForTimeout(700);
    await settle(page);
  }, after: async ({ page, held }) => {
    for (const route of held.splice(0)) await route.abort().catch(() => undefined);
    await page.unroute((url) => url.pathname === '/api/uploads').catch(() => undefined);
    await page.unrouteAll({ behavior: 'ignoreErrors' }).catch(() => undefined);
    const remove = page.getByRole('button', { name: /^Remove Launch-day support replies/ });
    for (let i = 0; i < 10 && await remove.count(); i++) await remove.first().click().catch(() => undefined);
    await clearComposer(page, 'sol');
  } },
  { name: 'recording', what: 'Recording a voice note (Cmd+Shift+M): the time, the level meter, cancel and send', run: async ({ page }) => {
    await conversation(page, 'sol');
    await field(page).click();
    await page.keyboard.press(`${MOD}+Shift+M`);
    const bar = page.getByRole('group', { name: 'Recording a voice message' });
    await bar.waitFor();
    await bar.getByRole('timer').filter({ hasText: /0:0[2-9]/ }).waitFor({ timeout: 15_000 });
    await settle(page, 100);
  }, after: async ({ page }) => {
    await page.keyboard.press('Escape');
    await page.getByRole('group', { name: 'Recording a voice message' }).waitFor({ state: 'hidden', timeout: 10_000 }).catch(() => undefined);
  } },
  { name: 'dictating', what: 'Dictating (Cmd+Shift+D): recording before the words go into the field', run: async ({ page }) => {
    await conversation(page, 'wren');
    await field(page).click();
    await page.keyboard.press(`${MOD}+Shift+D`);
    const bar = page.getByRole('group', { name: 'Dictating' });
    await bar.waitFor();
    await bar.getByRole('timer').filter({ hasText: /0:0[2-9]/ }).waitFor({ timeout: 15_000 });
    await settle(page, 100);
  } },
  { name: 'dictated', what: 'After dictating: the transcript in the field, ready to send (the Deepgram stand-in\'s fixed words)', run: async ({ page }) => {
    // Goes on from `dictating`: Enter stops the recording and the transcript is typed in.
    const bar = page.getByRole('group', { name: 'Dictating' });
    if (await bar.count()) await page.keyboard.press('Enter');
    else {
      await conversation(page, 'wren');
      await field(page).click();
      await page.keyboard.press(`${MOD}+Shift+D`);
      await bar.getByRole('timer').filter({ hasText: /0:0[2-9]/ }).waitFor({ timeout: 15_000 });
      await page.keyboard.press('Enter');
    }
    await page.waitForFunction(() => (document.querySelector('[data-composer-field]')?.value ?? '').length > 0, undefined, { timeout: 30_000 });
    await settle(page, 300);
  }, after: ({ page }) => clearComposer(page, 'wren') },
  { name: 'failed-send', what: 'A send that failed (no connection to DM after five tries): Retry and Discard', run: async ({ page }) => {
    await conversation(page, 'wren');
    const target = `/api/api/v1/conversations/${encodeURIComponent(address('wren'))}/messages`;
    await page.route((url) => decodeURIComponent(url.pathname) === decodeURIComponent(target), (route) => (route.request().method() === 'POST' ? route.abort('connectionreset') : route.continue()));
    await field(page).fill('Yes please, draft it and I\'ll sign it after lunch.');
    // The send button, not Enter: on a phone Enter is a new line.
    await page.getByRole('button', { name: 'Send', exact: true }).click();
    await page.getByRole('button', { name: /Discard/ }).first().waitFor({ timeout: 60_000 });
    await scrollThread(page, 'bottom');
    await settle(page, 300);
  }, after: async ({ page }) => {
    await page.getByRole('button', { name: /Discard/ }).first().click().catch(() => undefined);
    await page.unrouteAll({ behavior: 'ignoreErrors' }).catch(() => undefined);
    await clearComposer(page, 'wren');
  } },
  { name: 'offline', what: 'Offline: the banner over the thread', run: async ({ page, context }) => {
    await conversation(page, 'sol');
    await context.setOffline(true);
    await page.getByText('You are offline.').first().waitFor({ timeout: 20_000 });
    await settle(page, 300);
  }, after: ({ context }) => context.setOffline(false) },
  {
    name: 'listen-mode', what: 'Listen mode on: a message from Wren arrives and is spoken ("Speaking…", Stop)',
    // Every width listens at once, so one arriving message serves them all (each run adds one line to Wren's thread).
    together: true,
    prepare: async ({ page }) => {
      await conversation(page, 'wren');
      const toggle = page.getByRole('button', { name: 'Listen mode' }).first();
      await toggle.click();
      await page.waitForFunction(() => [...document.querySelectorAll('button[aria-pressed="true"]')].some((button) => /Listen mode/.test(button.getAttribute('aria-label') ?? button.textContent ?? '')), undefined, { timeout: 10_000 }).catch(() => undefined);
    },
    shared: () => {
      const line = nextWrenLine();
      cliSend('wren', who('mira').id, line);
      return line;
    },
    capture: async ({ page }, line) => {
      await row(page, line.slice(0, 30)).waitFor({ timeout: 30_000 });
      await page.getByRole('button', { name: 'Stop speaking' }).first().waitFor({ timeout: 30_000 });
      await page.waitForFunction(() => (document.querySelector('audio[data-dm-player]')?.currentTime ?? 0) > 0.8, undefined, { timeout: 30_000 }).catch(() => undefined);
      await settle(page, 100);
    },
    after: async ({ page }) => {
      await page.getByRole('button', { name: 'Stop speaking' }).first().click().catch(() => undefined);
      const toggle = page.getByRole('button', { name: 'Listen mode' }).first();
      if ((await toggle.getAttribute('aria-pressed').catch(() => null)) === 'true') await toggle.click().catch(() => undefined);
    },
  },
];

/**
 * What Wren says while Listen mode is taken: one line per run, in turn (the place is kept in .dev/demo/screens.json),
 * long enough to still be speaking while every width is captured, short enough for the CLI's 140-character rule.
 */
const WREN_LINES = [
  'Load test passed: two thousand bookings a minute for ten minutes, and not a single error.',
  'Release notes for build 1843 are merged. Sol has them for the launch post.',
  'The status page now shows the new region. I am watching the error rate for the next hour.',
  'I renewed the certificate for the booking domain. It is good until next October.',
  'Sign-ups from the beta list are coming in steadily, about one every three minutes.',
  'Android build 1844 is up for internal testing, signed with the new key.',
  'I cleared the old preview deployments. Twelve were left over from last week.',
  'Support has three new tickets, all about time zones. I tagged them for Thursday.',
  'The crash from this morning is a duplicate of last week\'s. I closed it and linked the two.',
  'Store screenshots are uploaded and waiting for review. Apple usually answers within a day.',
];
const harnessFile = join(DEV, 'demo', 'screens.json');
function nextWrenLine() {
  const kept = readJson(harnessFile, { listen: 0 });
  const line = WREN_LINES[kept.listen % WREN_LINES.length];
  writeJsonPrivate(harnessFile, { ...kept, listen: kept.listen + 1 });
  return line;
}

// ------------------------------------------------------------------------------------------------ main

async function main() {
  if (flag('list')) {
    for (const surface of SURFACES) process.stdout.write(`${surface.name.padEnd(26)} ${surface.what}\n`);
    return;
  }
  if (!out) throw new StackError('Say where the screenshots go: --out <folder>.', 'node scripts/chat-stack/screens.mjs --base http://127.0.0.1:4530 --out /tmp/dm-screens');
  if (!demo?.conversations?.sol) throw new StackError('There is no demo workspace yet (.dev/demo.json).', 'Make it first: node scripts/chat-stack/seed-demo.mjs');
  pool = withDemo(loadPool());
  if (pool.demo?.set !== demo.set) throw new StackError(`The demo accounts are set ${pool.demo?.set}, but .dev/demo.json describes set ${demo.set}.`, 'Run node scripts/chat-stack/seed-demo.mjs again.');
  const only = option('only') ? new Set(option('only').split(',')) : null;
  const skip = new Set((option('skip') ?? '').split(',').filter(Boolean));
  const surfaces = SURFACES.filter((surface) => (!only || only.has(surface.name)) && !skip.has(surface.name));
  if (only) for (const name of only) if (!SURFACES.some((surface) => surface.name === name)) throw new StackError(`There is no surface ${name}.`, 'node scripts/chat-stack/screens.mjs --list');
  const reach = await fetch(`${base}/robots.txt`).then((response) => response.status).catch(() => 0);
  if (reach !== 200) throw new StackError(`The website does not answer at ${base}.`, 'Start it (pnpm dev, or a production build with next start) and pass its address with --base.');

  mkdirSync(out, { recursive: true });
  const timezone = pickTimezone();
  say(`${surfaces.length} surfaces × ${widths.join(', ')} px from ${base} as ${who('mira').display_name} (${who('mira').id}), time zone ${timezone}, into ${out}`);
  const { chromium } = playwright();
  const browser = await chromium.launch({ args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', `--use-file-for-fake-audio-capture=${fakeMicrophone()}`, '--autoplay-policy=no-user-gesture-required'] });
  const results = new Map();
  const failures = [];
  const contexts = new Map();
  const options = (width) => ({
    baseURL: base, viewport: { width, height: HEIGHTS[width] ?? 900 }, deviceScaleFactor: width < 768 ? 2 : 1, isMobile: width < 768, hasTouch: width < 768,
    locale: 'en-US', timezoneId: timezone, colorScheme: 'light', permissions: ['microphone', 'clipboard-read', 'clipboard-write'],
  });
  /** A signed-in page per width: Mira's own sign-in (a new one per width, so two pages never share a refresh token). */
  async function signedIn(width) {
    if (contexts.has(width)) return contexts.get(width);
    const cookie = JSON.parse(spawnSync(process.execPath, [join(HERE, 'identities.mjs'), 'cookie', 'mira'], { encoding: 'utf8' }).stdout);
    const context = await browser.newContext(options(width));
    await context.addCookies([{ name: cookie.name, value: cookie.value, url: base, httpOnly: true, secure: false, sameSite: 'Lax' }]);
    // Receipts are held (never answered) in this browser: looking must not mark the demo read.
    const held = [];
    await context.route((url) => /\/receipts$/.test(url.pathname), (route) => held.push(route));
    const page = await context.newPage();
    page.setDefaultTimeout(20_000);
    const entry = { context, page, held: [], receipts: held, width };
    contexts.set(width, entry);
    return entry;
  }

  /** The picture, with the pointer resting in a corner (a hover would show a toolbar the surface is not about). */
  async function shoot(entry, surface, file) {
    await entry.page.mouse.move(2, 2).catch(() => undefined);
    await entry.page.waitForTimeout(150);
    await entry.page.screenshot({ path: join(out, file), fullPage: Boolean(surface.fullPage), animations: 'disabled', caret: 'hide' });
  }

  /** A surface every width shows at once: each prepares, one shared step (a message arriving), each is taken. */
  async function captureTogether(surface) {
    const ready = [];
    for (const width of widths) {
      try {
        const entry = await signedIn(width);
        await surface.prepare(entry);
        ready.push(entry);
      } catch (error) {
        failures.push({ surface: surface.name, width, error: error.message.split('\n')[0] });
        say(`${surface.name}-${width}.png FAILED: ${error.message.split('\n')[0]}`);
      }
    }
    try {
      const shared = ready.length ? await surface.shared(ready) : null;
      for (const entry of ready) {
        const file = `${surface.name}-${entry.width}.png`;
        try {
          await surface.capture(entry, shared);
          await shoot(entry, surface, file);
          results.set(file, true);
          say(file);
        } catch (error) {
          failures.push({ surface: surface.name, width: entry.width, error: error.message.split('\n')[0] });
          say(`${file} FAILED: ${error.message.split('\n')[0]}`);
          await entry.page.screenshot({ path: join(out, `${surface.name}-${entry.width}-failed.png`) }).catch(() => undefined);
          rmSync(join(out, file), { force: true });
        }
      }
    } finally {
      for (const entry of ready) {
        if (surface.after) await Promise.resolve(surface.after(entry)).catch(() => undefined);
        await entry.page.keyboard.press('Escape').catch(() => undefined);
      }
    }
  }

  await startPresence();
  try {
    for (const surface of surfaces) {
      if (surface.together) {
        await captureTogether(surface);
        continue;
      }
      for (const width of widths) {
        const file = `${surface.name}-${width}.png`;
        let entry = null;
        let temporary = null;
        try {
          if (surface.signedOut) {
            temporary = await browser.newContext(options(width));
            entry = { context: temporary, page: await temporary.newPage(), held: [], width };
          } else {
            entry = await signedIn(width);
          }
          await surface.run(entry);
          await shoot(entry, surface, file);
          results.set(file, true);
          say(`${file}`);
        } catch (error) {
          failures.push({ surface: surface.name, width, error: error.message.split('\n')[0] });
          say(`${file} FAILED: ${error.message.split('\n')[0]}`);
          if (entry?.page) await entry.page.screenshot({ path: join(out, `${surface.name}-${width}-failed.png`) }).catch(() => undefined);
          rmSync(join(out, file), { force: true });
        } finally {
          if (entry && surface.after) await Promise.resolve(surface.after(entry)).catch(() => undefined);
          if (entry && !surface.signedOut) {
            await entry.page.keyboard.press('Escape').catch(() => undefined);
            await entry.context.setOffline(false).catch(() => undefined);
          }
          await temporary?.close();
        }
      }
    }
  } finally {
    await stopPresence();
    for (const entry of contexts.values()) await entry.context.close().catch(() => undefined);
    await browser.close();
    // Listen mode left Wren's newest line unread; everything before it is read, so the inbox shows exactly one.
    try {
      const page = await dm('mira', 'GET', `/api/v1/conversations/${encodeURIComponent(address('wren'))}/messages?limit=5`);
      const fromWren = (page?.items ?? []).filter((item) => item.sender.id === who('wren').id && !item.deleted_at);
      if (fromWren.length > 1) await dm('mira', 'POST', `/api/v1/conversations/${encodeURIComponent(address('wren'))}/receipts`, { type: 'receipts.read', data: { up_to: fromWren[1]['message-id'] } });
    } catch (error) {
      say(`could not tidy Wren's unread messages: ${error.message}`);
    }
  }

  const lines = [
    '# DM screenshots: the demo workspace',
    '',
    `Taken ${new Date().toISOString()} from ${base} by \`scripts/chat-stack/screens.mjs\`, signed in as ${who('mira').display_name} (\`${who('mira').id}\`, demo set ${demo.set}),`,
    `in the time zone ${timezone}, with Chromium's fake microphone. The workspace comes from \`scripts/chat-stack/seed-demo.mjs\`; the`,
    'speech and transcripts come from the stack\'s Deepgram stand-in unless it ran with `DM_DEEPGRAM_REAL=1`.',
    '',
    `| Surface | ${widths.map((width) => `${width} × ${HEIGHTS[width] ?? 900}`).join(' | ')} | What it shows |`,
    `| --- | ${widths.map(() => '---').join(' | ')} | --- |`,
    // Every surface whose picture is in the folder, from this run or an earlier one (--only adds to a folder).
    ...SURFACES.filter((surface) => surfaces.includes(surface) || widths.some((width) => existsSync(join(out, `${surface.name}-${width}.png`)))).map((surface) => `| \`${surface.name}\` | ${widths.map((width) => {
      const file = `${surface.name}-${width}.png`;
      if (results.has(file) || (!surfaces.includes(surface) && existsSync(join(out, file)))) return `[${file}](${file})`;
      return surfaces.includes(surface) ? `failed: [${surface.name}-${width}-failed.png](${surface.name}-${width}-failed.png)` : 'not taken';
    }).join(' | ')} | ${surface.what}${surface.fullPage ? ' (full page)' : ''} |`),
    '',
  ];
  if (failures.length) lines.push('## Not taken', '', ...failures.map((failure) => `- \`${failure.surface}\` at ${failure.width}: ${failure.error}`), '');
  writeFileSync(join(out, 'index.md'), `${lines.join('\n')}`);
  process.stdout.write(`${JSON.stringify({ out, index: join(out, 'index.md'), taken: results.size, failed: failures }, null, 2)}\n`);
  if (failures.length) process.exitCode = 1;
}

main().catch((error) => {
  process.stderr.write(`error: ${error.message}\n${error.hint ? `hint: ${error.hint}\n` : ''}`);
  if (!(error instanceof StackError) && error.stack) process.stderr.write(`${error.stack.split('\n').slice(1, 4).join('\n')}\n`);
  process.exitCode = 1;
});
