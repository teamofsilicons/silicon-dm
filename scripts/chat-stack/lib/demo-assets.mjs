// The demo workspace's pictures and documents (scripts/chat-stack/seed-demo.mjs): small HTML pages rendered by the
// website's own Playwright Chromium into PNGs and a PDF that look like real work on a launch: two landing-page hero
// mock-ups, a signups chart, a whiteboard run of show and a two-page pricing document. They are made once and kept in
// .dev/demo/assets (named by a hash of their page, so a changed page is rendered again).
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { DEV, ROOT, StackError } from './common.mjs';

export const ASSETS_DIR = join(DEV, 'demo', 'assets');

const SANS = `"Avenir Next", "Helvetica Neue", Helvetica, Arial, sans-serif`;
const HAND = `"Marker Felt", "Chalkboard SE", "Noteworthy", "Comic Sans MS", cursive`;

/** A landing page's first screen in a browser window: the product the demo's team launches (Juniper). */
function hero({ variant }) {
  const dark = variant === 'a';
  const ink = dark ? '#F4EFE4' : '#1D2A22';
  const page = dark ? '#1F3D2B' : '#F6F1E7';
  const accent = dark ? '#E9C46A' : '#2F6B4F';
  const title = dark ? 'Your week,<br>finally calm.' : 'Booking that<br>runs itself.';
  const lede = dark
    ? 'Juniper is the calendar for small studios: clients book, reschedule and pay without a single email.'
    : 'Clients pick a time, Juniper confirms, reminds and fills the gaps. You get your evenings back.';
  const cta = dark ? 'Start free' : 'Try Juniper free';
  const art = dark
    ? `<div class="cal">${['Mon', 'Tue', 'Wed', 'Thu', 'Fri'].map((day, d) => `<div class="col"><b>${day}</b>${[0, 1, 2, 3].map((slot) => `<i class="${(d + slot) % 3 === 0 ? 'on' : (d * slot) % 4 === 1 ? 'soft' : ''}"></i>`).join('')}</div>`).join('')}</div>`
    : `<div class="phone"><div class="notch"></div><p class="today">Today</p>${[['9:00', 'Aisha · Deep tissue', 'on'], ['10:30', 'Marco · Intro call', ''], ['12:00', 'Lunch', 'muted'], ['14:15', 'Priya · Follow-up', 'on'], ['16:00', 'Open slot', 'open']].map(([time, what, kind]) => `<div class="slot ${kind}"><span>${time}</span><em>${what}</em></div>`).join('')}</div>`;
  return `<!doctype html><html><head><meta charset="utf-8"><style>
    * { box-sizing: border-box; margin: 0; }
    body { width: 1200px; height: 760px; background: #D9D4CA; font-family: ${SANS}; padding: 28px; }
    .window { width: 100%; height: 100%; border-radius: 14px; overflow: hidden; background: ${page}; box-shadow: 0 18px 40px rgba(0,0,0,.18); display: flex; flex-direction: column; }
    .bar { height: 38px; background: ${dark ? '#183223' : '#ECE6DA'}; display: flex; align-items: center; gap: 8px; padding: 0 16px; }
    .bar i { width: 12px; height: 12px; border-radius: 50%; background: ${dark ? '#355C45' : '#D2CABB'}; display: block; }
    .bar span { margin-left: 18px; font-size: 13px; color: ${dark ? '#9FB7A6' : '#7D7466'}; background: ${dark ? '#21402E' : '#F6F1E7'}; padding: 4px 14px; border-radius: 7px; }
    nav { display: flex; align-items: center; padding: 26px 56px; color: ${ink}; }
    nav strong { font-size: 24px; letter-spacing: -0.02em; display: flex; align-items: center; gap: 10px; }
    nav strong i { width: 26px; height: 26px; border-radius: 8px; background: ${accent}; display: block; }
    nav a { margin-left: 32px; font-size: 15px; opacity: .8; }
    nav a:first-of-type { margin-left: auto; }
    main { flex: 1; display: grid; grid-template-columns: 1.1fr 1fr; align-items: center; padding: 0 56px 40px; gap: 40px; }
    h1 { font-size: 66px; line-height: 1.02; letter-spacing: -0.035em; color: ${ink}; font-weight: 700; }
    p.lede { margin-top: 22px; font-size: 19px; line-height: 1.45; color: ${ink}; opacity: .82; max-width: 470px; }
    .cta { margin-top: 32px; display: flex; gap: 14px; align-items: center; }
    .cta b { background: ${accent}; color: ${dark ? '#1F3D2B' : '#F6F1E7'}; padding: 15px 26px; border-radius: 12px; font-size: 17px; }
    .cta span { color: ${ink}; font-size: 16px; opacity: .75; }
    .cal { display: grid; grid-template-columns: repeat(5, 1fr); gap: 10px; background: #F4EFE4; border-radius: 18px; padding: 22px; }
    .col b { display: block; font-size: 13px; color: #5D6B61; margin-bottom: 10px; }
    .col i { display: block; height: 62px; border-radius: 10px; background: #E4DDCF; margin-bottom: 10px; }
    .col i.on { background: #E9C46A; } .col i.soft { background: #A9C8B2; }
    .phone { justify-self: center; width: 300px; height: 540px; border-radius: 42px; background: #fff; border: 10px solid #1D2A22; padding: 28px 18px; position: relative; }
    .notch { position: absolute; top: 0; left: 50%; transform: translateX(-50%); width: 110px; height: 22px; border-radius: 0 0 14px 14px; background: #1D2A22; }
    .today { font-size: 26px; font-weight: 700; color: #1D2A22; margin: 10px 4px 16px; }
    .slot { display: flex; gap: 12px; align-items: center; padding: 14px 12px; border-radius: 12px; margin-bottom: 10px; background: #F3EFE6; font-size: 14px; color: #1D2A22; }
    .slot span { font-weight: 600; width: 46px; } .slot em { font-style: normal; }
    .slot.on { background: #DCEBE2; } .slot.muted { opacity: .55; } .slot.open { border: 2px dashed #2F6B4F; background: transparent; color: #2F6B4F; }
  </style></head><body><div class="window">
    <div class="bar"><i></i><i></i><i></i><span>juniper.app</span></div>
    <nav><strong><i></i>juniper</strong><a>Features</a><a>Pricing</a><a>Studios</a><a>Sign in</a></nav>
    <main><div><h1>${title}</h1><p class="lede">${lede}</p><div class="cta"><b>${cta}</b><span>No card needed</span></div></div>${art}</main>
  </div></body></html>`;
}

/** Signups per day, this week against last week (the newsletter went out on Tuesday). */
function chart() {
  const days = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  const thisWeek = [42, 131, 77, 64, 58];
  const lastWeek = [38, 44, 41, 47, 40, 22, 19];
  const left = 90;
  const top = 150;
  const width = 1020;
  const height = 430;
  const max = 140;
  const x = (index) => left + (width / (days.length - 1)) * index;
  const y = (value) => top + height - (value / max) * height;
  const line = (values) => values.map((value, index) => `${index ? 'L' : 'M'}${x(index).toFixed(1)},${y(value).toFixed(1)}`).join(' ');
  const grid = [0, 35, 70, 105, 140].map((value) => `<line x1="${left}" x2="${left + width}" y1="${y(value)}" y2="${y(value)}" stroke="#E7E9EE"/><text x="${left - 16}" y="${y(value) + 5}" text-anchor="end" class="tick">${value}</text>`).join('');
  const labels = days.map((day, index) => `<text x="${x(index)}" y="${top + height + 34}" text-anchor="middle" class="tick">${day}</text>`).join('');
  const dots = thisWeek.map((value, index) => `<circle cx="${x(index)}" cy="${y(value)}" r="6" fill="#2563B8" stroke="#fff" stroke-width="2.5"/>`).join('');
  return `<!doctype html><html><head><meta charset="utf-8"><style>
    * { margin: 0; box-sizing: border-box; } body { width: 1200px; height: 700px; background: #fff; font-family: ${SANS}; }
    .tick { font-size: 16px; fill: #6B7280; } .title { font-size: 28px; font-weight: 600; fill: #111827; } .sub { font-size: 17px; fill: #6B7280; }
    .legend { font-size: 16px; fill: #374151; } .note { font-size: 16px; font-weight: 600; fill: #1E3A8A; }
  </style></head><body><svg width="1200" height="700" viewBox="0 0 1200 700">
    <text x="${left - 40}" y="62" class="title">Signups per day</text>
    <text x="${left - 40}" y="94" class="sub">Juniper beta · Oct 12 to 18 against the week before</text>
    <g transform="translate(${left + width - 330}, 52)"><rect width="18" height="4" y="8" fill="#2563B8"/><text x="28" y="16" class="legend">This week (372 so far)</text>
      <line x1="0" x2="18" y1="40" y2="40" stroke="#9CA3AF" stroke-width="3" stroke-dasharray="5 4"/><text x="28" y="46" class="legend">Last week (251)</text></g>
    ${grid}${labels}
    <path d="${line(lastWeek)}" fill="none" stroke="#9CA3AF" stroke-width="3" stroke-dasharray="7 6"/>
    <path d="${line(thisWeek)} L${x(4)},${y(0)} L${x(0)},${y(0)} Z" fill="#2563B8" fill-opacity="0.08"/>
    <path d="${line(thisWeek)}" fill="none" stroke="#2563B8" stroke-width="4" stroke-linejoin="round"/>
    ${dots}
    <line x1="${x(1)}" x2="${x(1) + 70}" y1="${y(131) - 8}" y2="${y(131) - 40}" stroke="#1E3A8A" stroke-width="2"/>
    <text x="${x(1) + 78}" y="${y(131) - 38}" class="note">Newsletter went out</text>
  </svg></body></html>`;
}

/** The launch-day run of show, photographed off a whiteboard. */
function whiteboard() {
  const rows = [
    ['8:00', 'status page + monitors', true],
    ['9:00', 'press embargo lifts', true],
    ['9:30', 'newsletter (Sol)', false],
    ['10:00', 'standup, all hands on deck', false],
    ['12:00', 'post the launch thread?', false],
    ['15:00', 'retro + cake', false],
  ];
  const wobble = (seed) => Math.sin(seed * 12.9898) * 1.6;
  return `<!doctype html><html><head><meta charset="utf-8"><style>
    * { margin: 0; box-sizing: border-box; }
    body { width: 1200px; height: 800px; background: #6D6A63; display: grid; place-items: center; }
    .board { width: 1140px; height: 740px; border-radius: 6px; border: 14px solid #B9BCC0; background: radial-gradient(ellipse at 30% 20%, #FFFFFF 0%, #F3F4F1 55%, #E6E8E3 100%); position: relative; font-family: ${HAND}; color: #1F2937; padding: 46px 70px; transform: rotate(-0.6deg); box-shadow: inset 0 0 60px rgba(0,0,0,.06); }
    h1 { font-size: 54px; letter-spacing: 0.02em; color: #1E3A8A; font-weight: 400; }
    h1 u { text-decoration: none; border-bottom: 5px solid #DC2626; padding-bottom: 2px; }
    ul { list-style: none; margin-top: 34px; }
    li { font-size: 34px; margin: 14px 0; display: flex; gap: 22px; align-items: baseline; }
    li b { font-weight: 400; color: #DC2626; width: 108px; display: inline-block; }
    li s { text-decoration-thickness: 4px; text-decoration-color: #1E3A8A; }
    .box { display: inline-block; width: 28px; height: 28px; border: 4px solid #1F2937; border-radius: 4px; position: relative; top: 4px; }
    .box.done::after { content: "✓"; position: absolute; left: 2px; top: -16px; font-size: 38px; color: #15803D; }
    .sticky { position: absolute; right: 80px; top: 120px; width: 250px; height: 220px; background: #FDE68A; transform: rotate(4deg); box-shadow: 0 8px 16px rgba(0,0,0,.18); padding: 26px 22px; font-size: 32px; line-height: 1.2; color: #7C2D12; }
    .arrow { position: absolute; left: 556px; top: 172px; }
    .sign { position: absolute; right: 90px; bottom: 50px; font-size: 26px; color: #6B7280; }
    .tray { position: absolute; left: 0; right: 0; bottom: -14px; height: 14px; background: #9CA0A5; }
  </style></head><body><div class="board">
    <h1><u>LAUNCH DAY</u> · Thursday</h1>
    <ul>${rows.map(([time, what, done], index) => `<li style="transform: translateX(${wobble(index + 1)}px) rotate(${wobble(index + 7) / 3}deg)"><span class="box ${done ? 'done' : ''}"></span><b>${time}</b>${done ? `<s>${what}</s>` : what}</li>`).join('')}</ul>
    <div class="sticky">embargo is 9:00 sharp!!<br>(not 8:30)</div>
    <svg class="arrow" width="230" height="70" viewBox="0 0 230 70"><path d="M220 30 C 170 4, 80 4, 14 36" fill="none" stroke="#1E3A8A" stroke-width="5" stroke-linecap="round"/><path d="M38 20 L13 36 L40 48" fill="none" stroke="#1E3A8A" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/></svg>
    <div class="sign">Bo + Mira, Tuesday</div>
    <div class="tray"></div>
  </div></body></html>`;
}

/** Juniper's pricing, version 3: the plans on page one, Bo's notes on page two. */
function pricing() {
  const plans = [
    ['Solo', '$12', '$9.60', 'One practitioner, unlimited bookings, reminders by email and SMS'],
    ['Studio', '$29', '$23.20', 'Up to five people, shared calendar, deposits and packages'],
    ['Clinic', '$79', '$63.20', 'Unlimited people, rooms and equipment, intake forms, priority support'],
  ];
  return `<!doctype html><html><head><meta charset="utf-8"><style>
    @page { size: A4; margin: 22mm 20mm; }
    * { box-sizing: border-box; } body { font-family: ${SANS}; color: #1D2A22; font-size: 11.5pt; line-height: 1.5; }
    h1 { font-size: 26pt; letter-spacing: -0.02em; margin: 0 0 4pt; } .meta { color: #6B6455; margin-bottom: 22pt; }
    h2 { font-size: 14pt; margin: 22pt 0 8pt; } table { width: 100%; border-collapse: collapse; }
    th, td { text-align: left; padding: 9pt 8pt; border-bottom: 1px solid #DDD6C8; vertical-align: top; } th { font-size: 9.5pt; text-transform: uppercase; letter-spacing: .06em; color: #6B6455; }
    td.price { font-weight: 600; white-space: nowrap; } .page { break-after: page; } .note { background: #F6F1E7; border-left: 4px solid #2F6B4F; padding: 10pt 12pt; margin: 10pt 0; }
    ul { padding-left: 16pt; } li { margin: 6pt 0; }
  </style></head><body>
    <section class="page"><h1>Juniper pricing, v3</h1><p class="meta">Draft for sign-off · Mira Patel, Bo Okafor · October 2026</p>
      <p>Three plans, billed monthly or yearly. Yearly billing takes 20% off every plan. Every plan starts with a 30-day trial and no card.</p>
      <h2>Plans</h2><table><thead><tr><th>Plan</th><th>Monthly</th><th>Yearly, per month</th><th>What it includes</th></tr></thead>
      <tbody>${plans.map(([name, month, year, what]) => `<tr><td><b>${name}</b></td><td class="price">${month}</td><td class="price">${year}</td><td>${what}</td></tr>`).join('')}</tbody></table>
      <h2>What changed since v2</h2><ul><li>Studio drops from $34 to $29: most trials that stopped were studios of two or three people.</li><li>Clinic gains intake forms, which were the most requested feature in the beta.</li><li>The annual discount stays at 20%.</li></ul></section>
    <section><h1>Notes from Bo</h1><p class="meta">Design review, Tuesday</p>
      <div class="note">Put the yearly price first on the page and the monthly one under it: people compare per-month numbers.</div>
      <div class="note">Move the FAQ below the table, and keep it to five questions.</div>
      <div class="note">"Clinic" reads clinical next to the calm hero. Worth testing "Practice" before launch.</div>
      <h2>Open questions</h2><ul><li>Do we show prices in INR for India at launch, or the week after?</li><li>Who answers the pricing questions in support on launch day?</li></ul></section>
  </body></html>`;
}

/** Every asset: its file name in the messages, its type and how it is made. */
export const ASSETS = {
  heroA: { name: 'Hero A, dark.png', type: 'image/png', size: { width: 1200, height: 760 }, html: () => hero({ variant: 'a' }) },
  heroB: { name: 'Hero B, light.png', type: 'image/png', size: { width: 1200, height: 760 }, html: () => hero({ variant: 'b' }) },
  chart: { name: 'Signups this week.png', type: 'image/png', size: { width: 1200, height: 700 }, html: chart },
  whiteboard: { name: 'Run of show, whiteboard.png', type: 'image/png', size: { width: 1200, height: 800 }, html: whiteboard },
  pricing: { name: 'Juniper pricing v3.pdf', type: 'application/pdf', html: pricing },
};

/** The website's Playwright (web/node_modules): the same Chromium the browser tests use. */
export function playwright() {
  try {
    return createRequire(join(ROOT, 'web', 'package.json'))('@playwright/test');
  } catch (error) {
    throw new StackError(`The website's Playwright could not be loaded (${error.message}).`, 'Install the website: pnpm -C web install --frozen-lockfile; and its browser: pnpm -C web exec playwright install chromium');
  }
}

/**
 * Renders the assets that are missing (or whose page changed) and returns {key: {path, name, type, bytes, sha256}}.
 * A browser starts only when something has to be rendered.
 */
export async function renderAssets() {
  mkdirSync(ASSETS_DIR, { recursive: true, mode: 0o700 });
  const out = {};
  let browser = null;
  try {
    for (const [key, asset] of Object.entries(ASSETS)) {
      const html = asset.html();
      const tag = createHash('sha256').update(html).digest('hex').slice(0, 12);
      const path = join(ASSETS_DIR, `${key}-${tag}.${asset.type === 'application/pdf' ? 'pdf' : 'png'}`);
      if (!existsSync(path)) {
        browser ??= await playwright().chromium.launch();
        const page = await browser.newPage({ viewport: asset.size ?? { width: 1200, height: 800 }, deviceScaleFactor: 1 });
        await page.setContent(html, { waitUntil: 'load' });
        if (asset.type === 'application/pdf') writeFileSync(path, await page.pdf({ format: 'A4', printBackground: true, preferCSSPageSize: true }));
        else writeFileSync(path, await page.screenshot({ type: 'png' }));
        await page.close();
      }
      const bytes = readFileSync(path);
      out[key] = { path, name: asset.name, type: asset.type, bytes, sha256: createHash('sha256').update(bytes).digest('hex') };
    }
  } finally {
    await browser?.close();
  }
  return out;
}
