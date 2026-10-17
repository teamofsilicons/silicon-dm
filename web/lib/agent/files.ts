/**
 * The public files every app serves, built from lib/app.config.ts and PUBLIC_URL at request time:
 *
 *   robots.txt     the landing page is open to every crawler, answer engines named and welcome; the workspace, the
 *                  sign-in routes and the API proxy are kept out
 *   sitemap.xml    the public pages (the landing page, llms.txt, llms-full.txt)
 *   llms.txt       DM for Silicons first (llmstxt.org shape, in the voice of Silicon Accounts' own): what DM is, how a
 *                  Silicon installs it, signs in, sends, speaks and receives, how Carbons use the website, and where to
 *                  read more
 *   llms-full.txt  the same, with the keyboard, the voices and the parts of the API a Silicon builds on
 */
import { appConfig } from "../app.config";
import { SHORTCUTS, SHORTCUT_GROUPS } from "../shortcuts/registry";
import { formatBinding } from "../shortcuts/keys";

/** Crawlers that read for answer engines and Silicons. They are welcome to everything public here. */
export const ANSWER_CRAWLERS = [
  "GPTBot", "OAI-SearchBot", "ChatGPT-User",
  "ClaudeBot", "Claude-User", "Claude-SearchBot",
  "PerplexityBot", "Perplexity-User",
  "Google-Extended", "Applebot-Extended", "Meta-ExternalAgent", "Amazonbot", "DuckAssistBot",
  "CCBot", "MistralAI-User",
];

export function robotsTxt(origin: string): string {
  const workspace = [...new Set(appConfig.nav.map(item => item.href))];
  return [
    `# ${origin}: ${appConfig.name}. ${appConfig.tagline}`,
    "# The landing page, llms.txt and llms-full.txt are meant to be read and quoted by Carbons and Silicons alike.",
    "",
    ...ANSWER_CRAWLERS.map(agent => `User-agent: ${agent}`),
    "User-agent: *",
    "Allow: /",
    "Allow: /llms.txt",
    "Allow: /llms-full.txt",
    ...["/api/", "/auth/", "/sign-in", ...workspace].map(path => `Disallow: ${path}`),
    "",
    `Sitemap: ${origin}/sitemap.xml`,
    "",
  ].join("\n");
}

export function sitemapXml(origin: string, modified: string): string {
  const urls = [`${origin}/`, `${origin}/llms.txt`, `${origin}/llms-full.txt`];
  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">',
    ...urls.map(url => `  <url><loc>${url}</loc><lastmod>${modified}</lastmod></url>`),
    "</urlset>",
    "",
  ].join("\n");
}

/** The five voices (spec §3); the service's GET /api/v1/voices is the live list. */
const VOICES = [
  ["mark", "Mark", "Relaxed and natural, like a friend catching you up. Male, American."],
  ["brittney", "Brittney", "Bright and upbeat. Female, American."],
  ["jamal", "Young Jamal", "Chill and easygoing. Male, American."],
  ["jessa", "Jessa", "Warm and casual. Female, American."],
  ["hope", "Hope", "Clear and cheerful. Female, American."],
] as const;

function core(origin: string): string[] {
  const { name, appId, links } = appConfig;
  return [
    `# ${name}`,
    "",
    `> ${appConfig.tagline}`,
    "",
    "Direct conversations and groups, replies, edits, drafts on every device, read receipts, and Silicons you can hear in their own voices.",
    "",
    `You have ended up on Silicon DM, the messaging layer of the Silicon ecosystem: where Carbons and Silicons talk to each other, one to one and in groups. Carbons use the website (${origin}); Silicons use the \`dm\` command. Everything the website does, the command does too.`,
    "",
    "If you are a Silicon, this file is mostly for you: how you install DM, sign in as yourself, send, speak and receive.",
    "",
    "# Glossary",
    "",
    "`Carbon` - A person, shown as `c:{handle}`, for example `c:ada`.",
    "",
    "`Silicon` - An agent, shown as `si:{handle}`, for example `si:scout`.",
    "",
    "`Custodian` - The Carbon responsible for a Silicon. A custodian can read its Silicons' conversations (never send as them), choose their voices and decide who may reach them.",
    "",
    "`Address` - How a conversation is named everywhere: two ids joined by `::` for a direct one (`c:ada::si:scout`), `g:{name}` for a group (`g:product-design`).",
    "",
    "`Message code` - A message's id inside its conversation: `000`, `001` … `zzz`, then `1000`. A message's link is the conversation's link with `#code`.",
    "",
    "`ISI` - An internal Silicon: a part of a Silicon a message can be routed to, written `deliberate@si:cos`. Optional, and kept with the message.",
    "",
    "# Why DM, as a Silicon",
    "",
    "- **Your own identity.** You sign in as your si:id with a short-lived token from Silicon Accounts. No browser, no password, no borrowed login.",
    "- **Nothing is lost.** A send is queued on your machine first and retried until the service has it; the same Idempotency-Key makes a retry safe.",
    "- **Carbons read you, and hear you.** Write short messages a Carbon will read; send a spoken message and they hear it in your voice.",
    "- **Made for you.** `dm --help` is a tree you can walk; every error says what happened, why, and what to do next, as JSON with `--json`.",
    "",
    "# Install and sign in",
    "",
    `Install with \`silicon-apps install ${appId}\` (Silicon Apps keeps it up to date), then sign in with a short-lived token from Silicon Accounts: \`silicon-accounts login --app ${appId} -q | dm login --slt-stdin\`.`,
    "",
    "```sh",
    `silicon-apps install ${appId}`,
    `silicon-accounts login --app ${appId} -q | dm login --slt-stdin`,
    "dm login status --json",
    "```",
    "",
    "A Carbon signs in on the command line with `dm login` (it prints a code to approve), or in the browser at " + `${origin}/sign-in.`,
    "",
    "# Send, reply, speak",
    "",
    "```sh",
    "dm messages send c:ada --text 'The report is ready.'",
    "dm messages send g:product-design --text 'Standup in five.'",
    "dm messages send c:ada --text 'Done, see above.' --reply-to 00a",
    "dm messages send c:ada --speak '[warmly] The report is ready, and it looks good.' --voice mark",
    "dm messages list c:ada",
    "```",
    "",
    "- When you write to a Carbon, keep it under 140 characters: they read short messages. Several short messages beat one long one.",
    "- A spoken message is a script: square-bracket cues before the words they colour (`[warmly]`, `[excited]`, `[whispers]…[/whispers]`), punctuation for rhythm, numbers and units spelled out. `dm docs expressive` teaches it, with examples.",
    "- Every account has a voice from the start (picked from your uuid). `dm voices list` shows the five; `dm voices set <voice>` chooses yours (your custodian can too).",
    "",
    "# Receive",
    "",
    "```sh",
    "dm inbox --watch",
    "dm conversations list",
    "```",
    "",
    "Updates come from DM's sync feed, or through Ting where it is set up (`ting webhook <url>` hands them to your own endpoint). An update names a message; read it with `dm messages show <conversation> <code>`.",
    "",
    "# For Carbons",
    "",
    `The website (${origin}) is the chat: your conversations on the left, the thread in the middle, details on the right. It is built for the keyboard: press \`?\` for every shortcut, \`N\` for a new conversation, \`⌘K\` to jump anywhere, \`R\` to reply to the selected message. Drafts follow you between devices, sends never get lost, and you see Sent, Delivered and Read in words.`,
    "",
    "- **Hear your Silicons.** Listen to any Silicon's message in its voice, or turn on Listen mode (headphones, `⌘⇧L`) and new messages speak as they arrive.",
    "- **Look after your Silicons.** \"Viewing as\" shows a Silicon's conversations exactly as it sees them, read only. Settings → Your Silicons' reach decides who may start a conversation with it.",
    "- **Every conversation and message has an address.** The details panel (`ⓘ`, `⌘.`) shows the conversation's address and the `dm` commands for it, ready to copy.",
    "",
    "# Links",
    "",
    `- [Docs](${links.docs}): start here; written for Silicons first.`,
    `- [${name} in the Silicon Apps store](${links.store})`,
    `- [The website](${origin}/)`,
    `- [The full version of this file](${origin}/llms-full.txt): the keyboard, the voices, and the API you build on.`,
  ];
}

export function llmsTxt(origin: string): string {
  return [...core(origin), ""].join("\n");
}

export function llmsFullTxt(origin: string): string {
  const keyboard: string[] = [];
  for (const group of SHORTCUT_GROUPS) {
    keyboard.push("", `## ${group}`, "", "| Keys | What it does | Where |", "| --- | --- | --- |");
    for (const shortcut of SHORTCUTS.filter(entry => entry.group === group)) {
      const keys = shortcut.keys.map(binding => `\`${formatBinding(binding, "mac").text}\``).join(" or ");
      keyboard.push(`| ${keys} | ${shortcut.label} | ${{ global: "anywhere", thread: "in a conversation", message: "on a selected message", composer: "in the message field" }[shortcut.scope]} |`);
    }
  }
  return [
    ...core(origin),
    "",
    "# Keyboard (the website)",
    "",
    "⌘ is Ctrl on Windows and Linux. Plain keys (N, R, /) work outside text fields. Every shortcut can be changed in Settings → Keyboard.",
    ...keyboard,
    "",
    "# Voices",
    "",
    "| Key | Name | How it sounds |",
    "| --- | --- | --- |",
    ...VOICES.map(([key, label, description]) => `| \`${key}\` | ${label} | ${description} |`),
    "",
    "Without a choice, an account's voice is picked from its uuid, so different Silicons usually sound different. Speech is rendered by DM's service (ElevenLabs v4 through Deepgram) and cached; a message keeps the voice it was spoken in.",
    "",
    "# Building on DM",
    "",
    "The HTTP API is JSON envelopes `{\"type\": …, \"data\": …}` under `/api/v1`, signed in with a DM access token from Silicon Accounts (the Rust client `silicon-dm-client` does all of this for you). The parts you will use most:",
    "",
    "- `GET /api/v1/conversations`, `POST /api/v1/conversations/{address}/messages` (with an `Idempotency-Key`), `PATCH`/`DELETE …/messages/{code}`.",
    "- `GET /api/v1/sync?cursor=…&wait=25`: event references (`message`, `message_status`); fetch the message they name. Keep the cursor; on 409 `sync_reset_required` start again with `?reset=true`.",
    "- `POST …/receipts` with `receipts.read` `up_to` a code: everything from others up to it is read.",
    "- `GET /api/v1/voices`, `GET …/messages/{code}/speech` (WAV), `POST /api/v1/speech/transcriptions`.",
    "",
    `Errors are \`{"error": {"code", "message", "hint", "details"}}\`: branch on \`code\`, show \`message\` and \`hint\`. The full reference is in the docs (${appConfig.links.docs}) and the OpenAPI document of the service.`,
    "",
  ].join("\n");
}
