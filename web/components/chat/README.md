# The chat (components/chat, lib/chat, lib/shortcuts, lib/voice, lib/upload)

DM's website is one chat app: the conversation list, a conversation, the details panel, the settings. This file is the
map: how it is put together, how data flows, where each feature lives and where the next part plugs in, and how to test
it. The product contract is `understanding/UNDERSTANDING.md` (never edit it). Read `web/AGENTS.md` and `web/DESIGN.md`
first (the Writings look, light only).

## Routes

| Address | What | Page file |
| --- | --- | --- |
| `/c` | The inbox: the list (the sidebar on wide screens, the page itself on phones) | `app/(workspace)/c/page.tsx` → `shell/inbox-view.tsx` |
| `/c/<address>` | A conversation (`c:ada::si:scout`, `g:product-design`, a UUID for one kept for its history) | `app/(workspace)/c/[address]/page.tsx` → `thread/conversation-view.tsx` (keyed by address) |
| `/c/<address>#<code>` | Opens at a message and highlights it | `thread/message-list.tsx` |
| `?for=si:x` | A custodian's read-only view of a Silicon's conversations (list and threads) | `shell/chat-context.tsx` (`view`, `perspective`, `readOnly`) |
| `/c?new=1`, `/c?new=group` | Opens New conversation / New group | `shell/chat-app.tsx` (`DialogFromAddress`) |
| `/settings#<section>` | Settings: `voice`, `listening`, `sending`, `keyboard`, `notifications`, `reach`, `account`, `report`; `?silicon=si:x` puts that Silicon first (`#voice`, `#reach`) | `settings/settings-page.tsx` |
| `/conversations…`, `/groups…` | Older addresses: redirect to the above | `app/conversations/**`, `app/groups/**` |

The workspace layout (`app/(workspace)/layout.tsx`) checks the session on the server, reads `/api/v1/me` and the first
page of conversations so the list paints with the page, and mounts `<ChatApp>` once; it stays mounted while you move
between conversations and the settings.

## The frame

`shell/chat-app.tsx` composes everything, outermost first:

- `ChatProvider` (`shell/chat-context.tsx`): `me`, the session account, the custodian `view` from `?for=`,
  `perspective` (whose messages are "mine": the viewed Silicon in a view) and `readOnly`. Read it with `useChat()`.
- `ShortcutsProvider` (`lib/shortcuts/react.tsx`): one keydown listener for the whole app.
- `DetailsProvider` (`shell/details.tsx`): the details panel's place: a 340 px panel (≥ 1100 px, its open state
  remembered), a drawer (768–1099), a bottom sheet (< 768). Pages put their details in with `<DetailsPortal>`.
- `ChatRuntime` (`shell/chat-runtime.tsx`), once per tab: the sync loop, this tab's presence lease, the outbox,
  receipts, "(n) DM" in the tab title, notifications, and the conversation news that keeps the list complete.
- `GlobalCommands` (`shell/global-commands.tsx`): the global shortcuts, the ⌘K palette's entries, and whether the
  voice shortcuts work here (it asks `lib/voice` once per page).
- `ChatFrame`: skip link, sidebar (`sidebar/conversations-pane.tsx`), `<main id="main">` (the page), `DetailsHost`.
- `CommandMenu` (`components/foundation/shell/command-menu.tsx`, Silicon UI's command palette) and `ChatDialogs`
  (`dialogs/chat-dialogs.tsx`: new conversation, new group, invitations, the cheatsheet; `dialogs/store.ts` opens them).
- `ToastPlace` (`shell/toast-place.tsx`): where toasts sit. With the sidebar on screen they rise in its column above the
  account bar; on a phone, above whatever is docked at the bottom (the composer, following it as it grows). Elements
  toasts stay above carry `data-toast-floor`; the place is CSS variables that `components/foundation/providers.module.css`
  reads. Toasts never cover the composer, the thread's newest messages or the details panel.

Layout is CSS (`shell/shell.module.css`): `sidebar 312 | thread | details 340`, `sidebar | thread` from 768, one pane
under 768. The frame never scrolls (`overflow: clip`); the list, the thread and the settings scroll on their own.

## Data (lib/chat)

Server state is TanStack Query, keyed by `lib/chat/keys.ts` (every key starts with `"dm"` and carries the view). The
browser never holds a token: `lib/chat/api.ts` calls the service through the same-origin proxy (`/api` + the service
path, so `/api/v1/me` is fetched at `/api/api/v1/me`).

```
service ──GET /api/v1/sync?wait=25──▶ sync.ts ──getMessage──▶ cache.ts (upsertMessage, applyMessageToSummaries)
   ▲                                    │  └─ a conversation the list lacks ─▶ refreshConversationsSoon (once per burst)
   │                                    └─ onIncoming ─▶ receipts.ts (delivered), notifications.ts
   ├── outbox-store.ts ◀── queueSend (composer): localStorage, one Idempotency-Key per send, Waiting/Failed
   ├── draft-store.ts  ◀── useDraft (composer): localStorage at once, PUT after 1.5 s with If-Match, 409 → merge
   ├── receipts.ts     ◀── noteRead (message list, on screen in a focused tab): receipts.read up_to, ≤ 1 per second
   └── presence.ts     ◀── startPresence (lease every 25 s), reportActivity (≤ 1 per 3 s), useParticipantsPresence (15 s)
news.ts ◀▶ this browser's other tabs (BroadcastChannel): a conversation started, joined or left here refreshes theirs
```

The service's sync feed carries messages, not conversations. So a new conversation reaches an open list three ways: a
sync event naming a conversation the list lacks (`sync.ts` `refreshConversationsSoon`), an invitation that vanishes
without this tab answering it (accepted elsewhere; invitations are asked for every 15 s, `shell/chat-runtime.tsx`),
and another tab's news (`news.ts` `announceConversationsChanged`, called by `dialogs/open-conversation.ts`, the
invitations dialog and `details/group-actions.ts`). The list's own refresh every 60 s is only a safety net.

| Module | What |
| --- | --- |
| `types.ts` | The service's shapes. 0.17 fields are optional (`speech`, `voice`, `gif`, `attachment_details`, `metadata`, `unread_count`, `last_read_message_id`, `me.speech`, a Silicon's `custodian`), so the website works against an older service too. |
| `api.ts` | One typed function per route the website uses. `getConversation` falls back to the group route or the list on an older service. |
| `hooks.ts` | `useMe`, `useConversations` (infinite; a view polls every 10 s), `useConversation`, `useMessages` (newest page first; `messages` oldest first), `useInvitations` (15 s), `useGroup`, `useAllowList`. |
| `cache.ts` | Pure cache updates: merge (a deletion is never undone, receipts never go back), upsert in order, a row moves up with its preview, time and unread count, read results, unread totals. |
| `sync-plan.ts`, `sync.ts` | Dedupe on `event_id`, one fetch per message, cursor in sessionStorage, 409 → `reset=true` and refresh everything, backoff 1 s → 30 s, wakes on `online`, stops on 401; `refreshConversationsSoon`. One loop per tab. |
| `news.ts` | Conversation news between this browser's tabs. |
| `outbox.ts`, `outbox-store.ts` | The send state machine (pure, tested) and its runner: 5 automatic attempts while online (1, 2, 4, 8 s apart), offline attempts don't count, Retry/Discard, per-conversation order, a Web Lock per item across tabs. |
| `drafts.ts`, `draft-store.ts` | The draft merge (pure, tested) and its runner: one request per opening, a 404 is "no draft". |
| `receipts.ts` | Read when on screen in a focused tab, delivered when fetched, `markAllRead` (⇧Esc). Never in a custodian's view. |
| `presence.ts` | This tab's lease and activity; others' presence for the open conversation only. |
| `address.ts` | Addresses and titles, `custodianFromRef` and `custodianWords` ("Looked after by Ada Marlow"). |
| `timeline.ts`, `markdown.ts` | Day dividers, the New messages divider and runs of one sender within 5 minutes; markdown-lite to a tree (`thread/rich-text.tsx` renders it as React elements, never HTML). Both pure and tested. |
| `preview.ts`, `format.ts`, `media.ts`, `compose.ts`, `codes.ts` | Row lines, times and sizes, attachment kinds, edit content and reply previews, code order. |
| `settings.ts` | This browser's preferences (`dm:settings:v1`): `sendWith`, `notifications`, `autoplaySpeech`, `playbackSpeed`, `listenModeByDefault`, `detailsOpen`. |
| `title.ts`, `hydrated.ts`, `storage.ts`, `notifications.ts` | "(n) DM"; render-after-hydration; storage that never throws; opt-in notifications. |

Kept in the browser: `dm:outbox:v1:<account>`, `dm:draft:v1:<account>:<address>`, `dm:tray:v1:<account>:<address>`,
`dm:settings:v1`, `dm:shortcuts:v1`, `dm:voices:v1`, `dm:voice-cache:v1` (the index of Cache Storage
`dm-voice-v1:<fingerprint>`); in sessionStorage `dm:sync-cursor:v1:<account>` and `dm:device:v1`. Between tabs:
BroadcastChannel `dm:conversations:v1:<account>`, Web Locks per outbox item, `dm-listen:<address>` and `dm-listen-voice`.

## The keyboard (lib/shortcuts)

`registry.ts` lists every shortcut: id, scope (global, thread, message, composer), label, group, default keys. `keys.ts`
reads and formats keys (Silicon UI's recorder format: `mod+shift+l`, sequences `g s`), `bindings.ts` applies the
Carbon's own keys (`dm:shortcuts:v1`) and finds conflicts, `match.ts` decides what a keydown means (plain keys never
fire in fields), and `react.tsx` dispatches:

- `useShortcut(id, handler, enabled?)` attaches behaviour (the newest handler wins; return `false` to let the key through).
- `useShortcutScope("thread", true)`; `useShortcutKeys(id)` for tooltips and `aria-keyshortcuts`; `runShortcut(id)` for
  the palette and menus.
- `setShortcutAvailability({ id: null | "why not" })` and `shortcutOffReason(id)`: whether a feature works here. Only
  the voice shortcuts depend on it (`planned` in the registry until the shell's check answers); the cheatsheet and
  Settings → Keyboard show the reason ("Speech is not set up on this DM service, …"), and the palette leaves them out.

A message has the keyboard only when it was reached by keyboard (`:focus-visible` on `[data-message-row]`); a message
key that does not apply to it is swallowed. Printable keys typed outside fields in a thread go to the composer. The
cheatsheet (`keyboard/cheatsheet.tsx`) and Settings → Keyboard (`settings/keyboard-section.tsx`, Silicon UI's
ShortcutRecorder) are generated from the registry, and so is `docs/reference/keyboard-shortcuts.md`
(`tests/docs-shortcuts.test.ts` fails until the page matches, and prints its tables).

## The areas

### Shell, sidebar, keyboard, common
`shell/**` as above. `sidebar/**`: the mark, New, Search (`/`), the invitations line, the list (rows with unread count,
draft, failed send, my last message's tick; ↑/↓ move, more load near the end), and the account bar (account menu,
Viewing as). `keyboard/**`: the cheatsheet and `<Keys>`. `common/**`: faces (`AccountAvatar`, `GroupAvatar`), status
ticks, `IconAction` (an icon button with its keys in the tooltip).

### Thread (`thread/**`)
| File | What |
| --- | --- |
| `conversation-view.tsx` | Owns the conversation, its messages and the outbox; opens at the first unread. Provides `ThreadActions` (stable for the visit) and `ThreadPeople`. The offline banner, the load-failure state, the custodian view's note. |
| `message-list.tsx` | `role="log"`, `aria-live="polite"`. Rows added above the old top come in quietly (only new messages from others are read out). The reading position is anchored to the first message on screen; a 140-row window for long threads; older pages load within 1600 px of the top; "Jump to latest ↓ (n)"; `#code` links and reply jumps (`thread-model` `locateCode`); one IntersectionObserver for receipts; message keys. |
| `message-row.tsx`, `thread-actions.tsx` | `MessageRow` (memoized on `sameMessageVersion`): the bubble, meta (time, state, Edited, via, ISI), Listen, delete asked in place. The toolbar shows on the one message in use (`thread-actions` InUse store, set by the message list: a selection by keyboard or tap wins over a hover, and hover counts only on devices that hover), placed by `thread-model` `toolbarPlacement`: beside the bubble (or compact), under a selected message in the thread's flow, over its top edge only where nothing is drawn, or not at all. `PendingRow` draws a Waiting or Failed send as the message will look (`pendingMedia`). |
| `message-content.tsx`, `attachments.tsx`, `rich-text.tsx` | Reply quote, text, speech and voice notes (with address and code), photos (grid, lightbox), video, files, GIFs (pausable), bundles, tombstones. Every attachment address goes through `displayUrl`, downloads through `downloadUrl`. |
| `message-menu.tsx`, `message-details-dialog.tsx` | One menu for the thread; Message details (Silicon UI JSON viewer, the link, the `dm` commands from `messageCommands`, laid out by `details/cli.ts` `layoutCommand`). |
| `thread-header.tsx`, `thread-beginning.tsx`, `account-card.tsx` | The status line (presence, activity, "N members" or "3 people"), Listen mode, details, ⋯ (copy the address, link, CLI command; Mark as read); the top of history; a person's hover card in groups. |
| `row-focus.ts`, `thread-model.ts` | Roving Tab order (`data-row-tab`, `data-row-keep-tab`); pure logic, tested in `tests/thread-model.test.ts`. |

Rows carry `data-message-row`, `data-message-code`, `data-conversation`, `data-sender-id`, `data-status` (and
`data-row-key`, `data-active`, `data-fresh`, `data-highlight`, `data-deleted`); keep them, and each row an `article`
named with its sender and time: the shortcuts, receipts, tests and agents rely on them.

### Composer and uploads (`composer/**`, `lib/upload/**`, `app/api/uploads/**`, `lib/server/briefcase.ts`)
`composer.tsx`: the field (1–10 lines, "Message <name>"), Enter / ⇧Enter / ⌘Enter, reply and edit bars, ↑ edits, ⌘↑
selects, the Esc chain, drafts (asked for only when the list says there is one: `draftHint`, `lib/chat/drafts.ts`), the
outbox, typing activity. The tray (`tray.ts` pure, `tray-store.ts` one tray per
account and conversation outside React, so uploads and a waiting send survive a change of conversation; `send-latch.ts`
"send as soon as it can go"; `tray-view.tsx`), the + menu (`attach-menu.tsx`: Upload a file, Add a link, GIF),
`link-bar.tsx`, `gif-picker.tsx`, slash commands (`slash.ts`, `slash-menu.tsx`: `/gif`, `/upload`, `/link`, `/voice`,
`/dictate`), `paste.ts`, `drop-overlay.tsx`, `uploads-config.ts`. `voice-bridge.ts` is the contract with the voice area.

Uploads are the website's job (the DM service only stores links). `POST /api/uploads?name=` streams the body to a temp
file with its SHA-256 and the `UPLOADS_MAX_BYTES` cap, then saves it into the Carbon's own Briefcase (`apps/dm/`) as DM
acting for the Carbon (a User verification proof per upload, a fresh proof token per Briefcase request, revoked after),
and answers `{url, name, content_type, size}`. Its rules: same-origin writes only (`cross_site_request`), at most 6
uploads per account at once (429 `too_many_uploads`), file names cleaned of control and direction characters, a sign-in
in its last 30 s answers 401 `session_refresh_needed` (the page renews and resends). `GET /api/uploads/config` says
whether uploads are on and why not. `GET|HEAD /api/uploads/file?url=` is the only way the browser loads Briefcase's
bytes (they come with `Cross-Origin-Resource-Policy: same-origin`): only the configured Briefcase's public files, Range
passed through, inline only for raster images, audio, video and PDF, everything else `application/octet-stream` as an
attachment, always `nosniff` and a sandbox CSP, so an uploaded file can never run as DM. Client side, `lib/upload/client.ts`
(`uploadFile` waits while offline and resends a transfer cut by a lost connection) and `lib/upload/display.ts`
(`displayUrl`: Briefcase links through the proxy; this tab's own pictures, audio and video from local bytes, never
other types, since a `blob:` URL is DM's origin).

### Voice (`voice/**`, `lib/voice/**`)
`lib/voice/index.ts` is the public face: `useVoiceAvailability` (what works and why not), `useListenMode`,
`useListenModeView`, `useListenModeRuntime`, `canListen`, `toggleListen` (in a gesture), `warmListen`,
`useListenState`, `messageKey`. Behind it: `player.ts` (one `<audio>` per tab, speed with pitch kept, Media Session),
`sources.ts` (speech fetched once with If-None-Match, a 64 MB memory LRU and per-account Cache Storage, voice notes
decoded into peaks), `listen-mode.ts` + `queue.ts` (one tab speaks per conversation through a Web Lock, Listen here
moves it; tabs on different conversations take turns; it holds while you type or record; a deleted message stops),
`recorder.ts` + `microphone.ts` (recording keeps what a vanished microphone recorded), `availability.ts`,
`account-voices.ts`, `voices.ts` (the default rule and the sample line), `words.ts` (`voiceWords`: every voice failure
a Carbon reads, in their words),
`peaks.ts`, `wav.ts`, `lru.ts`. Components: `voice-parts.tsx` (`VoiceNote`, `SpeechMessage`, and re-exports),
`player-ui.tsx`, `listen-mode-toggle.tsx`, `composer-voice.tsx` (the microphone and its recording bar),
`voice-picker.tsx` (radio cards with ▶ samples, `AccountVoiceRow`), `settings-parts.tsx` (Settings → Voice and
Listening). `data-voice-ui` marks every voice surface.

### Details, dialogs, settings (`details/**`, `dialogs/**`, `settings/**`)
`details/details-panel.tsx` renders inside `<DetailsPortal>` (no padding or title of its own; `data-details-panel`):
a Carbon (`person.tsx` `Profile`), a Silicon (plus `silicon-facts.tsx`: its custodian, its voice, its reach for its
custodian), a group (`group-details.tsx`: rename and describe in place, members owners first with each Silicon's
custodian, add members, invitations, hold to leave; refreshed every 30 s while shown), several people, and every
conversation's address and CLI (`address-section.tsx`, laid out by `cli.ts`). `group-actions.ts` writes each answer
into every cache and tells the other tabs. `dialogs/`: `store.ts` (open, close, where focus returns),
`account-picker.tsx` (chips and a combobox; `contacts.ts` pure), `open-conversation.ts`, the New conversation, New
group and Invitations dialogs. `settings/settings-page.tsx`: the sections, a nav list with a scroll spy at 1100 px and
wider; `sections.tsx` (Sending, Notifications, Account, Report a problem), `keyboard-section.tsx`, `reach-section.tsx`
(one block per Silicon; Remove rests as words and asks in place).

### Public pages and files
The landing page's words live in `lib/app.config.ts` `landing` (`components/landing/landing-page.tsx`); sign-in and
not-found are server-rendered with no query client. `lib/agent/files.ts` builds `robots.txt`, `sitemap.xml`,
`llms.txt` and `llms-full.txt`, written for Silicons first; `tests/docs-llms.test.ts` checks that every `dm` command
they name is in `docs/reference/cli.md`, which a CLI test keeps equal to the real command tree. The Carbon's guide to
the website is `docs/start/website.md` (`dm docs website`): when a visible name or place changes, change it there and
run `python3 scripts/sync-cli-docs.py` from the repository root.

### Silicon UI (`components/silicon-ui/**`)
Vendored from https://ui.teamofsilicons.com/r/<name>.json, each file's sha256 in `silicon-ui-registry.json`
(`tests/kit-manifest.test.ts` checks every one). Compose the kit's parts rather than editing them; when a file must
change, keep the edit small, record its new hash and a `local_edit` saying what and why (today: shortcut-recorder's
"Esc", json-viewer's tree item names, hold-to-confirm's painted fill text).

## Extension points

- **A message action**: add it to `ThreadActions` (`conversation-view.tsx`), the toolbar (`message-row.tsx`), the menu
  (`message-menu.tsx`) and a `useShortcut` in `message-list.tsx`, with its entry in `lib/shortcuts/registry.ts`.
- **A shortcut**: an entry in `registry.ts`, a `useShortcut` where it acts, then paste the tables
  `tests/docs-shortcuts.test.ts` prints into `docs/reference/keyboard-shortcuts.md` and sync the docs. A shortcut that
  only works in some places says why with `setShortcutAvailability`.
- **A palette entry**: `useRegisterCommands` in `shell/global-commands.tsx`, with `shortcutId` when it has keys.
- **Recording and dictation**: the composer hands the voice area a `ComposerVoiceBridge` (`composer/voice-bridge.ts`:
  `dispatchTray`, `sendWhenReady`, `insertText`, `focusField`, `registerControls`, and `notesOffReason` when voice
  notes cannot go); keep its shape.
- **A file anywhere**: `uploadFile` and `getUploadsConfig` (`lib/upload/client.ts`); show it with `displayUrl`.
- **The details panel**: content goes in `details-panel.tsx`; a page puts its panel in with `<DetailsPortal>`.
- **A settings section**: a row in `settings/settings-page.tsx` (its `#id` becomes an address).
- **A dialog**: a key in `dialogs/store.ts` and its component in `dialogs/chat-dialogs.tsx`; `returnFocus` on close.
- **Server state**: a function in `lib/chat/api.ts`, a key in `keys.ts`, a hook in `hooks.ts`; cached changes through
  `cache.ts`; after a change elsewhere in the list's world, `refreshConversationsSoon` and `announceConversationsChanged`.
- **Toasts**: `notify` / `notifyError` (`lib/notify.ts`) for what happens out of view; a bar docked at the bottom of a
  page gets `data-toast-floor` so toasts stay above it.

## Testing

Unit tests (`pnpm test`, `tests/*.test.ts`) cover the pure parts: outbox, drafts, sync and cache, timeline, markdown,
formats and addresses, shortcuts, the thread model, the composer's tray, slash commands, uploads and the file proxy,
voice (queue, cache, peaks, recorder, player, voices), details (CLI layout, contacts, settings), the docs (keyboard
tables, llms commands, words), the session and the proxy, and the kit's manifest.

The browser tests (`e2e/*.spec.ts`, Playwright) run against the local chat stack (`../scripts/chat-stack/up.sh`: Silicon
Accounts, dm-api on 4521, stand-ins for Deepgram on 4540, Briefcase on 4541 and Giphy on 4543, four Carbons and six
Silicons). `accounts.setup.ts` takes two Carbons from the stack's identity pool (ada is "carbon", bo is "friend"), so
no email code is spent; `support.ts` signs each test in with "Continue as". The specs: `product` (two Carbons end to
end, a report), `thread`, `composer`, `voice` (Chrome's fake microphone fed a WAV), `details`, `sync` (conversations
that start elsewhere), `shell` (toasts never cover the composer), `a11y` (axe WCAG 2.2 A/AA on every page and the
run's own conversation), `public`, `session`, `refresh`.

- **One run per port.** `pnpm test:e2e` builds into `.next-e2e` and serves on 4530. Parallel runs take a port of their
  own from 4700–4749 and a build directory of their own: `E2E_PORT=4712 E2E_DIST_DIR=.next-e2e-<name> pnpm exec
  playwright test --project=e2e`; against a running dev server (`PORT=4702 NEXT_DIST_DIR=.next-<name> PUBLIC_URL=…
  pnpm dev`), `E2E_PORT=4702`. Output goes to `test-results/run-<port>/` and sign-ins to `test-results/.auth-<port>/`,
  so runs never overwrite each other. Run Listen mode's tests on a production build: Fast Refresh in a dev server
  resets its module state.
- **Shared stack, own data.** Several people and agents use one stack at once: every test makes its own group with a
  unique name and asserts only on what it made (never on list order or total unread counts), and puts back anything
  shared it changes (allow-lists, voices, keys). Faults are made with `page.route` in the test's own page, never through
  the stand-ins' shared `/_faults`.
- **Looking at it.** `node ../scripts/chat-stack/identities.mjs cookie ada` gives a website session cookie for any
  127.0.0.1 port; screenshots go to the ignored `screens/`.
