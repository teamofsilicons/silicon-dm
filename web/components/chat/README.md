# The chat (components/chat, lib/chat, lib/shortcuts, lib/voice)

DM's website is one chat app: the conversation list, a conversation, the details panel, the settings. This file is the
map: how it is put together, how data flows, where each feature lives, where the next parts plug in, and who owns
which directory. The product contract is `understanding/UNDERSTANDING.md`; the build spec for 0.17 is the session's
SPEC (§6 for the website). Read `web/AGENTS.md` and `web/DESIGN.md` (its top note: the Writings look, light only) first.

## Routes

| Address | What | Page file |
| --- | --- | --- |
| `/c` | The inbox: the list (sidebar on wide screens, the page itself on phones) | `app/(workspace)/c/page.tsx` → `shell/inbox-view.tsx` |
| `/c/<address>` | A conversation (`c:ada::si:scout`, `g:product-design`, a UUID for one kept for its history) | `app/(workspace)/c/[address]/page.tsx` → `thread/conversation-view.tsx` (keyed by address) |
| `/c/<address>#<code>` | Opens at a message and highlights it | `thread/message-list.tsx` (`jumpTo`) |
| `?for=si:x` | A custodian's read-only view of a Silicon's conversations (list and threads) | `shell/chat-context.tsx` (`view`, `perspective`, `readOnly`) |
| `/c?new=1`, `/c?new=group` | Opens New conversation / New group | `shell/chat-app.tsx` (`DialogFromAddress`) |
| `/settings` (`#voice`, `#listening`, `#sending`, `#keyboard`, `#notifications`, `#reach`, `#account`, `#report`) | Settings | `settings/settings-page.tsx` |
| `/conversations…`, `/groups…` | Older addresses: redirect to the above | `app/conversations/**`, `app/groups/**` |

The workspace layout (`app/(workspace)/layout.tsx`) checks the session on the server, reads `/api/v1/me` and the first
page of conversations so the list paints with the page, and mounts `<ChatApp>` once; it stays mounted while you move
between conversations and the settings.

## The frame

`shell/chat-app.tsx` composes everything, outermost first:

- `ChatProvider` (`shell/chat-context.tsx`): `me` (`/api/v1/me`), the session account, the custodian `view` from `?for=`,
  `perspective` (whose messages are "mine": the viewed Silicon in a view) and `readOnly`. Read it with `useChat()`.
- `ShortcutsProvider` (`lib/shortcuts/react.tsx`): one keydown listener for the whole app.
- `DetailsProvider` (`shell/details.tsx`): where the details panel shows — a 340 px panel (≥ 1100 px, open state
  remembered), a drawer (768–1099), a bottom sheet (< 768). Pages put their details in with `<DetailsPortal>`.
- `ChatRuntime` (`shell/chat-runtime.tsx`): what runs once per tab — sync loop, presence lease, outbox, receipts, the
  unread count in the tab title, browser notifications.
- `GlobalCommands` (`shell/global-commands.tsx`): the global shortcuts and the ⌘K palette's entries.
- `ChatFrame`: skip link, sidebar (`sidebar/conversations-pane.tsx`), `<main id="main">` (the page), `DetailsHost`.
- `CommandMenu` (`components/foundation/shell/command-menu.tsx`, Silicon UI's command palette) and `ChatDialogs`
  (`dialogs/chat-dialogs.tsx`: new conversation, new group, invitations, the keyboard cheatsheet; `dialogs/store.ts`
  opens them).

Layout is CSS (`shell/shell.module.css`): `sidebar 312 | thread | details 340`, `sidebar | thread` from 768, one pane
under 768. The frame never scrolls (`overflow: clip`); the list, the thread and the settings scroll on their own.

## Data flow (lib/chat)

Server state is TanStack Query; everything is keyed by `lib/chat/keys.ts` (every key starts with `"dm"` and carries the
view). The browser never holds a token: `lib/chat/api.ts` calls the service through the same-origin proxy (`/api` +
the service path, so `/api/v1/me` is fetched at `/api/api/v1/me`).

```
service ──GET /api/v1/sync?wait=25──▶ sync.ts ──getMessage──▶ cache.ts (upsertMessage, applyMessageToSummaries)
   ▲                                     │                         │
   │                                     └─ onIncoming ─▶ receipts.ts (delivered), notifications.ts
   │
   ├── outbox-store.ts ◀── queueSend (composer) — localStorage, retries with one Idempotency-Key, Waiting/Failed
   ├── draft-store.ts  ◀── useDraft (composer) — localStorage now, PUT draft after 1.5 s with If-Match, 409 → merge
   ├── receipts.ts     ◀── noteRead (message list, viewport + focus) — receipts.read up_to, 404 → single receipts
   └── presence.ts     ◀── startPresence (lease every 25 s), reportActivity (≤ 1 per 3 s), useParticipantsPresence (15 s)
```

| Module | What |
| --- | --- |
| `types.ts` | The service's shapes. 0.17 fields (`speech`, `voice`, `gif`, `attachment_details`, `metadata`, `unread_count`, `last_read_message_id`, `me.speech`) are optional. |
| `api.ts` | One typed function per route the website uses, including §4's (voices, speech WAV, transcription, `receipts` up_to). `getConversation` falls back to the group route or the list on an older service. |
| `hooks.ts` | `useMe`, `useConversations` (infinite, safety refresh 60 s; a view polls 10 s), `useConversation`, `useMessages` (infinite, newest page first; `messages` oldest first), `useInvitations`, `useGroup`, `useAllowList`. |
| `cache.ts` | Pure cache updates: merge (a deletion is never undone, receipts never go back), upsert in order, a row moves up with its new preview/time/unread, read results, unread totals. |
| `sync-plan.ts`, `sync.ts` | Dedupe on `event_id`, one fetch per message, cursor in sessionStorage, 409 → `reset=true` + refresh everything, backoff 1 s → 30 s, wakes on `online`, stops on 401. One loop per tab. |
| `outbox.ts`, `outbox-store.ts` | The send state machine (pure, tested) and its runner: 5 automatic attempts while online, offline attempts don't count, Retry/Discard, per-conversation order, Web Lock per item across tabs. |
| `drafts.ts`, `draft-store.ts` | The draft merge (pure, tested) and its runner. |
| `receipts.ts` | Read when on screen in a focused tab (batched ≤ 1/s), delivered when fetched, `markAllRead` (⇧Esc). Never in a custodian's view. |
| `presence.ts` | This tab's lease and activity; others' presence for the open conversation only. |
| `timeline.ts` | Day dividers, the New messages divider, runs of one sender within 5 min (pure, tested). |
| `markdown.ts` | Markdown-lite to a tree (pure, tested, linear); `thread/rich-text.tsx` renders it as React elements. |
| `preview.ts`, `format.ts`, `media.ts`, `compose.ts`, `address.ts`, `codes.ts` | Row lines, times and sizes, attachment kinds, edit content and reply previews, addresses and titles, code order. |
| `settings.ts` | This browser's preferences (`dm:settings:v1`): Enter vs ⌘Enter, notifications, listening, details open. |
| `title.ts`, `hydrated.ts`, `storage.ts`, `notifications.ts` | "(n) DM" in the tab title; render-after-hydration; storage that never throws; opt-in notifications. |

Storage keys: `dm:outbox:v1:<account>`, `dm:draft:v1:<account>:<address>`, `dm:settings:v1`, `dm:shortcuts:v1`,
`dm:sync-cursor:v1:<account>` (session), `dm:device:v1` (session).

## The keyboard (lib/shortcuts)

`registry.ts` lists every shortcut (id, scope, label, group, default keys; `planned` until its feature arrives).
`keys.ts` reads and formats keys (Silicon UI's recorder format: `mod+shift+l`, sequences `g s`), `bindings.ts` applies
the Carbon's own keys (`dm:shortcuts:v1`) and finds conflicts, `match.ts` decides what a keydown means (scope, plain keys
never fire in fields, sequences), `react.tsx` dispatches: `useShortcut(id, handler)` attaches behaviour (return `false`
to let the key through), `useShortcutScope("thread", true)`, `useShortcutKeys(id)` for tooltips and
`aria-keyshortcuts`, `provideShortcuts(ids)` turns a `planned` shortcut on. A message has the keyboard only when it was
reached by keyboard (`:focus-visible` on `[data-message-row]`). Printable keys typed outside fields in a thread go to the
composer (after the shortcuts had their turn). The cheatsheet (`keyboard/cheatsheet.tsx`) and Settings → Keyboard
(`settings/keyboard-section.tsx`, Silicon UI's ShortcutRecorder) are generated from the registry.

## Where each feature lives

| Feature | Files |
| --- | --- |
| Conversation list, search, invitations entry, Viewing as, account menu | `sidebar/**` |
| Thread header (presence line, Listen mode, details, more) | `thread/thread-header.tsx` |
| Messages: grouping, dividers, positions, older pages, Jump to latest, deep links, receipts, message keys | `thread/message-list.tsx` |
| A message: bubble, meta (time, Edited + history, via, ISI, state), toolbar, Failed + Retry/Discard, delete in place | `thread/message-row.tsx` |
| Bubble content: reply quote, text, attachments (gallery + lightbox, video, audio, files), GIF, bundles, tombstone | `thread/message-content.tsx`, `thread/attachments.tsx`, `thread/rich-text.tsx` |
| Message menu, message details (JSON + CLI) | `thread/message-menu.tsx`, `thread/message-details-dialog.tsx` |
| Composer: field, Enter/⌘Enter, reply and edit bars, ↑ edits, Esc, drafts, typing, outbox | `composer/composer.tsx`; tray model `composer/tray.ts` |
| Voice: voice notes, spoken messages, Listen mode toggle, microphone, Voice/Listening settings | `voice/**` + `lib/voice/index.ts` (first, plain versions) |
| Details panel | `details/details-panel.tsx` |
| Settings | `settings/**` |
| Dialogs: new conversation, new group, invitations | `dialogs/**` |
| Shared parts: faces, status ticks, icon actions with keys | `common/**` |

## Extension points for phase 2

- **Composer** — `<ComposerActions side="start|end">` around the field: the + menu (Upload file ⌘⇧U, Add a link, GIF)
  replaces the disabled + button; the tray model (`composer/tray.ts`: `trayReducer`, `trayPayload`, `trayBusy`) holds
  pending items and the composer already sends `trayPayload(tray)` with the words and waits while `trayBusy`. Slash
  commands go in the field's `onKeyDown` before the shortcuts (Esc sends the text literally). Report activity with
  `reportActivity("uploading_file" | "searching_gifs" | …)`. Uploads get new routes in `app/api/uploads/**` and a
  server helper in `lib/server/briefcase.ts` (neither exists yet). Briefcase answers public bytes with
  `Cross-Origin-Resource-Policy: same-origin`, so attachment URLs from Briefcase must be served through the website (a
  proxy route forwarding Range); `thread/attachments.tsx` shows any https URL as is.
- **Voice** — keep the exports of `lib/voice/index.ts` (`useVoiceAvailability`, `useListenMode`, `canListen`,
  `toggleListen`) and of `voice/voice-parts.tsx` (`VoiceNote`, `SpeechMessage`, `ListenModeToggle`, `ComposerVoice`) and
  `voice/settings-parts.tsx` (`VoiceSettings`, `ListeningSettings`); change everything behind them. Register the voice
  shortcuts with `useShortcut("listen.mode" | "composer.record" | "composer.dictate" | "message.listen", …)` and call
  `provideShortcuts([...])` when the feature is there. `lib/chat/api.ts` already has `fetchMessageSpeech`,
  `renderSpeech`, `transcribe`, `listVoices`, `getVoice`, `setVoice`, `resetVoice`; `proxy.ts` allows the microphone for
  this site.
- **Thread** — `MessageRow`/`PendingRow` take their actions from `thread-actions.tsx` (`useThreadActions`); add an
  action there and to the toolbar, the menu and the registry together. Keep `role="log"`, the `article` labels and the
  `data-message-row`, `data-message-code`, `data-conversation`, `data-sender-id`, `data-status` attributes (the
  shortcuts, receipts and tests rely on them).
- **Details and settings** — `details/details-panel.tsx` renders inside whatever `DetailsHost` gives it (no own
  padding or title); settings sections are rows in `settings/settings-page.tsx`.

## Ownership after phase 1

| Directory | Owner |
| --- | --- |
| `components/chat/thread/**` | thread |
| `components/chat/composer/**`, `app/api/uploads/**` (new), `lib/server/briefcase.ts` (new) | composer |
| `components/chat/voice/**`, `lib/voice/**` | voice |
| `components/chat/details/**`, `components/chat/settings/**`, `components/chat/dialogs/**` | details |
| `lib/chat/**`, `lib/shortcuts/**`, `components/chat/shell/**`, `components/chat/sidebar/**`, `components/chat/common/**`, `components/chat/keyboard/**` | foundation (frozen; changes by request) |
| `components/silicon-ui/**` (vendored; hashes in `silicon-ui-registry.json`) | foundation (by request) |

## Testing

Pure parts are unit-tested under `tests/` (`pnpm test`): outbox, drafts, sync and cache, timeline, markdown, shortcuts,
formats. `e2e/product.spec.ts` walks the chat with two Carbons against the local chat stack
(`scripts/chat-stack/up.sh`); `e2e/a11y.spec.ts` runs axe WCAG 2.2 AA on the chat's pages and dialogs.
