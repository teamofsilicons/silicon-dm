/**
 * Every keyboard shortcut of the website, in one place: what it does (the words the cheatsheet, the palette and the
 * tooltips show), where it works (its scope), and its default keys (spec §6.7). Components attach the behaviour with
 * `useShortcut(id, handler)` (lib/shortcuts/react.tsx); Settings → Keyboard rebinds them (lib/shortcuts/store.ts).
 *
 * Scopes:
 *   global    anywhere in the signed-in website (plain keys such as N never fire while typing in a field)
 *   thread    while a conversation is open
 *   message   while a message has the keyboard (↑/↓ through the thread, or ⌘↑ from the composer)
 *   composer  inside the message field
 *
 * A shortcut whose feature arrives with a later part of the website is listed with `planned`: the cheatsheet shows
 * it switched off, with that reason, until its module calls `provideShortcuts([...ids])`.
 *
 * Pure: tests/shortcuts.test.ts checks the defaults have no conflicts.
 */

export type ShortcutScope = "global" | "thread" | "message" | "composer";

export interface ShortcutDefinition {
  id: string;
  scope: ShortcutScope;
  /** What it does, as the cheatsheet and the palette say it ("New conversation"). */
  label: string;
  /** The cheatsheet's section. */
  group: "General" | "Conversations" | "Messages" | "Composer" | "Voice";
  /** Default bindings (any of them works), in the format of lib/shortcuts/keys.ts. */
  keys: string[];
  /** Why it is not there yet, until its module provides it. */
  planned?: string;
  /** Settings → Keyboard does not offer to rebind it (Enter to send follows Settings → Sending instead). */
  fixed?: boolean;
  /** Extra words for the cheatsheet's search. */
  keywords?: string;
}

const VOICE = "Arrives with voice notes and listening";
const UPLOADS = "Arrives with attachments";

export const SHORTCUTS: ShortcutDefinition[] = [
  // General
  { id: "palette.open", scope: "global", group: "General", label: "Command palette", keys: ["mod+k"], keywords: "search jump run" },
  { id: "search.focus", scope: "global", group: "General", label: "Search conversations", keys: ["/"], keywords: "find filter" },
  { id: "shortcuts.open", scope: "global", group: "General", label: "Keyboard shortcuts", keys: ["shift+/", "mod+/"], keywords: "cheatsheet help keys" },
  { id: "details.toggle", scope: "global", group: "General", label: "Show or hide the details panel", keys: ["mod+."], keywords: "info participants" },
  { id: "go.settings", scope: "global", group: "General", label: "Settings", keys: ["g s"], keywords: "preferences" },
  { id: "go.voice", scope: "global", group: "General", label: "Voice settings", keys: ["g v"], keywords: "voices speak" },
  { id: "go.invitations", scope: "global", group: "General", label: "Invitations", keys: ["g i"], keywords: "groups invites" },
  // Conversations
  { id: "conversation.new", scope: "global", group: "Conversations", label: "New conversation", keys: ["n"], keywords: "start message" },
  { id: "group.new", scope: "global", group: "Conversations", label: "New group", keys: ["shift+n"], keywords: "create group" },
  { id: "conversation.previous", scope: "global", group: "Conversations", label: "Previous conversation", keys: ["alt+up"] },
  { id: "conversation.next", scope: "global", group: "Conversations", label: "Next conversation", keys: ["alt+down"] },
  { id: "conversation.previousUnread", scope: "global", group: "Conversations", label: "Previous unread conversation", keys: ["alt+shift+up"] },
  { id: "conversation.nextUnread", scope: "global", group: "Conversations", label: "Next unread conversation", keys: ["alt+shift+down"] },
  { id: "conversations.markAllRead", scope: "global", group: "Conversations", label: "Mark everything read", keys: ["shift+escape"], keywords: "clear unread" },
  // Composer
  { id: "composer.send", scope: "composer", group: "Composer", label: "Send", keys: ["enter"], fixed: true, keywords: "submit" },
  { id: "composer.newline", scope: "composer", group: "Composer", label: "New line", keys: ["shift+enter"], fixed: true },
  { id: "composer.editLast", scope: "composer", group: "Composer", label: "Edit your last message (empty field)", keys: ["up"] },
  { id: "composer.selectLast", scope: "composer", group: "Composer", label: "Select the last message", keys: ["mod+up"], keywords: "message navigation" },
  { id: "composer.cancel", scope: "composer", group: "Composer", label: "Cancel reply or edit, then leave the field", keys: ["escape"] },
  { id: "composer.upload", scope: "composer", group: "Composer", label: "Upload a file", keys: ["mod+shift+u"], planned: UPLOADS, keywords: "attach" },
  // Messages
  { id: "message.previous", scope: "message", group: "Messages", label: "Previous message", keys: ["up", "k"] },
  { id: "message.next", scope: "message", group: "Messages", label: "Next message", keys: ["down", "j"] },
  { id: "message.reply", scope: "message", group: "Messages", label: "Reply", keys: ["r"] },
  { id: "message.edit", scope: "message", group: "Messages", label: "Edit (your message)", keys: ["e"] },
  { id: "message.copy", scope: "message", group: "Messages", label: "Copy text", keys: ["c"] },
  { id: "message.copyLink", scope: "message", group: "Messages", label: "Copy link", keys: ["l"] },
  { id: "message.delete", scope: "message", group: "Messages", label: "Delete (asks first)", keys: ["delete", "backspace"] },
  { id: "message.details", scope: "message", group: "Messages", label: "Message details", keys: ["i"], keywords: "json cli" },
  { id: "message.jumpLatest", scope: "message", group: "Messages", label: "Jump to latest", keys: ["end"] },
  { id: "message.back", scope: "message", group: "Messages", label: "Back to the composer", keys: ["escape"] },
  // Voice (the voice area provides these)
  { id: "listen.mode", scope: "global", group: "Voice", label: "Listen mode", keys: ["mod+shift+l"], planned: VOICE, keywords: "speak headphones" },
  { id: "composer.record", scope: "composer", group: "Voice", label: "Record a voice note (Enter sends, Esc cancels)", keys: ["mod+shift+m"], planned: VOICE, keywords: "microphone" },
  { id: "composer.dictate", scope: "composer", group: "Voice", label: "Dictate", keys: ["mod+shift+d"], planned: VOICE, keywords: "speech to text" },
  { id: "message.listen", scope: "message", group: "Voice", label: "Listen or pause", keys: ["space"], planned: VOICE, keywords: "play" },
];

export const SHORTCUT_GROUPS: Array<ShortcutDefinition["group"]> = ["General", "Conversations", "Composer", "Messages", "Voice"];

const byId = new Map(SHORTCUTS.map(definition => [definition.id, definition]));

export function shortcutDefinition(id: string): ShortcutDefinition | undefined {
  return byId.get(id);
}

/** The words for a scope, as Settings → Keyboard says where a shortcut works. */
export function scopeWords(scope: ShortcutScope): string {
  return { global: "Anywhere", thread: "In a conversation", message: "On a selected message", composer: "In the message field" }[scope];
}

/**
 * Whether two scopes can be active at once (then one key must not do two things). Global is active everywhere; a
 * message and the message field never have the keyboard together.
 */
export function scopesOverlap(a: ShortcutScope, b: ShortcutScope): boolean {
  if (a === b || a === "global" || b === "global") return true;
  if (a === "thread" || b === "thread") return true;
  return false;
}
