"use client";

/**
 * The shortcuts that work anywhere (spec §6.7) and the command palette's entries (spec §6.7: conversations, actions,
 * custodian views, each with its keys). Both run the same actions, so ⌘K finds everything the keyboard can do.
 */
import { useRouter } from "next/navigation";
import { useCallback, useEffect } from "react";
import { AudioLines, BookOpen, CircleUserRound, Copy, Eye, Headphones, Inbox, Keyboard, LogOut, Mail, MessageCircle, MessageSquarePlus, Mic, PanelRight, Settings2, Terminal, Users, Volume2 } from "lucide-react";
import { appConfig } from "@/lib/app.config";
import { accountName, conversationHref, conversationKindWords, conversationTitle, inboxHref, shellQuote } from "@/lib/chat/address";
import { unreadOf } from "@/lib/chat/cache";
import { useConversations } from "@/lib/chat/hooks";
import { markAllRead } from "@/lib/chat/receipts";
import { useSignOut } from "@/lib/client/session";
import { toggleCommandPalette, useRegisterCommands, type ShellCommand } from "@/lib/commands";
import { notify, notifyError } from "@/lib/notify";
import { isShortcutAvailable, runShortcut, setShortcutAvailability, useShortcut, useShortcutAvailability } from "@/lib/shortcuts/react";
import { useVoiceAvailability } from "@/lib/voice";
import { openDialog } from "../dialogs/store";
import { useOpenAddress } from "../sidebar/conversations-pane";
import { useChat } from "./chat-context";
import { useDetails } from "./details";

const icon = (Icon: typeof Inbox) => <Icon size={16} strokeWidth={1.75} />;

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    notify.success(`${what} copied`, text);
  } catch {
    notify.error({ status: 0, code: "clipboard_refused", message: "This browser did not let the page copy.", hint: "Select the text and copy it yourself." }, `Could not copy the ${what.toLowerCase()}`);
  }
}

/**
 * Whether the voice shortcuts work here, asked once for every page of the chat: the service's speech, uploads and the
 * browser's microphone (lib/voice). Without it the cheatsheet and Settings → Keyboard, opened before any conversation,
 * would not know; with it, an off shortcut says why ("Speech is not set up on this DM service…").
 */
function useVoiceShortcutAvailability(): void {
  const voice = useVoiceAvailability();
  const listen = voice.listen ? null : voice.listenReason ?? voice.reason;
  const record = voice.notes ? null : voice.notesReason ?? voice.reason;
  const dictate = voice.dictate ? null : voice.dictateReason ?? voice.reason;
  useEffect(() => {
    setShortcutAvailability({ "listen.mode": listen, "message.listen": listen, "composer.record": record, "composer.dictate": dictate });
  }, [listen, record, dictate]);
}

/** Focuses the conversation search that is on screen (the sidebar's, or the inbox page's on a phone). */
export function focusConversationSearch(): boolean {
  const fields = Array.from(document.querySelectorAll<HTMLInputElement>("[data-conversation-search]"));
  const visible = fields.find(field => field.offsetParent !== null) ?? null;
  if (!visible) return false;
  visible.focus();
  visible.select();
  return true;
}

export function GlobalCommands() {
  const { me, view, readOnly, perspective, accountsUrl } = useChat();
  const router = useRouter();
  const { items } = useConversations(view);
  const openAddress = useOpenAddress();
  const details = useDetails();
  const { signOut } = useSignOut();
  // Voice actions appear in the palette once they work here (deps below re-register then).
  useVoiceShortcutAvailability();
  const available = useShortcutAvailability();

  const go = useCallback((href: string) => router.push(href), [router]);

  const step = useCallback((direction: 1 | -1, unreadOnly: boolean) => {
    if (!items.length) return;
    const at = items.findIndex(item => item.id === openAddress);
    for (let offset = 1; offset <= items.length; offset++) {
      const index = at < 0 ? (direction === 1 ? offset - 1 : items.length - offset) : (at + direction * offset + items.length * 2) % items.length;
      const candidate = items[index]!;
      if (unreadOnly && !unreadOf(candidate, perspective)) continue;
      go(conversationHref(candidate.id, { view }));
      return;
    }
    if (unreadOnly) notify.info("Nothing unread", "Every conversation is read.");
  }, [items, openAddress, perspective, view, go]);

  useShortcut("palette.open", () => toggleCommandPalette());
  useShortcut("search.focus", () => (focusConversationSearch() ? undefined : false));
  useShortcut("shortcuts.open", () => openDialog("shortcuts"));
  useShortcut("conversation.new", () => openDialog("new-conversation"), !readOnly);
  useShortcut("group.new", () => openDialog("new-group"), !readOnly);
  useShortcut("conversation.previous", () => step(-1, false));
  useShortcut("conversation.next", () => step(1, false));
  useShortcut("conversation.previousUnread", () => step(-1, true));
  useShortcut("conversation.nextUnread", () => step(1, true));
  useShortcut("conversations.markAllRead", () => {
    void markAllRead(items.filter(item => unreadOf(item, perspective) > 0)).then(count => {
      if (count) notify.success("Everything is read", count === 1 ? "1 conversation marked read." : `${count} conversations marked read.`);
    });
  }, !readOnly);
  useShortcut("go.settings", () => go("/settings"));
  useShortcut("go.voice", () => go("/settings#voice"));
  useShortcut("go.invitations", () => openDialog("invitations"));

  const open = items.find(item => item.id === openAddress) ?? null;

  useRegisterCommands(() => {
    const commands: ShellCommand[] = [];
    if (open) {
      // As the thread header copies it: a custodian's view is read only, so its command reads (your own sign-in can).
      const cli = view ? `dm messages list ${shellQuote(open.id)}` : `dm messages send ${shellQuote(open.id)} --text 'hello'`;
      commands.push(
        { id: "conversation.copyAddress", label: "Copy this conversation's address", description: open.id, group: "This conversation", icon: icon(Copy), keywords: ["address", "id", "link"], run: () => void copy(open.id, "Address") },
        { id: "conversation.copyCli", label: "Copy its CLI command", description: view ? cli : `dm messages send ${shellQuote(open.id)} --text …`, group: "This conversation", icon: icon(Terminal), keywords: ["cli", "terminal", "dm"], run: () => void copy(cli, "CLI command") },
        { id: "conversation.copyLink", label: "Copy its link", description: `${window.location.origin}${conversationHref(open.id, { view })}`, group: "This conversation", icon: icon(Copy), keywords: ["url", "share"], run: () => void copy(`${window.location.origin}${conversationHref(open.id, { view })}`, "Link") },
        { id: "details.toggle", label: details.open ? "Hide the details panel" : "Show the details panel", group: "This conversation", icon: icon(PanelRight), shortcutId: "details.toggle", run: details.toggle },
      );
    }
    commands.push(
      { id: "conversation.new", label: "New conversation", group: "Actions", icon: icon(MessageSquarePlus), shortcutId: "conversation.new", keywords: ["start", "message", "write"], run: () => openDialog("new-conversation") },
      { id: "group.new", label: "New group", group: "Actions", icon: icon(Users), shortcutId: "group.new", keywords: ["create"], run: () => openDialog("new-group") },
      { id: "go.invitations", label: "Invitations", group: "Actions", icon: icon(Mail), shortcutId: "go.invitations", keywords: ["groups", "invites", "accept"], run: () => openDialog("invitations") },
      { id: "conversations.markAllRead", label: "Mark everything read", group: "Actions", icon: icon(Inbox), shortcutId: "conversations.markAllRead", keywords: ["clear", "unread"], run: () => void markAllRead(items.filter(item => unreadOf(item, perspective) > 0)) },
      { id: "shortcuts.open", label: "Keyboard shortcuts", group: "Actions", icon: icon(Keyboard), shortcutId: "shortcuts.open", keywords: ["keys", "cheatsheet", "help"], run: () => openDialog("shortcuts") },
      { id: "go.settings", label: "Settings", group: "Go to", icon: icon(Settings2), shortcutId: "go.settings", keywords: ["preferences", "sending", "notifications"], run: () => go("/settings") },
      { id: "go.voice", label: "Voice settings", group: "Go to", icon: icon(Volume2), shortcutId: "go.voice", keywords: ["voices", "speak", "listening"], run: () => go("/settings#voice") },
      { id: "go.keyboard", label: "Change keyboard shortcuts", group: "Go to", icon: icon(Keyboard), keywords: ["rebind", "keys"], run: () => go("/settings#keyboard") },
      { id: "go.docs", label: `${appConfig.name} docs`, description: "Opens in a new tab", group: "Go to", icon: icon(BookOpen), keywords: ["help", "guide", "cli"], run: () => void window.open(appConfig.links.docs, "_blank", "noopener") },
      { id: "go.account", label: "Your account", description: "Silicon Accounts, in a new tab", group: "Go to", icon: icon(CircleUserRound), keywords: ["profile", "photo"], run: () => void window.open(accountsUrl, "_blank", "noopener") },
      { id: "account.signout", label: "Sign out", description: `Sign this browser out of ${appConfig.name}`, group: "Account", icon: icon(LogOut), keywords: ["log out", "logout"], run: () => void signOut().catch(failure => notifyError(failure, "Could not sign out")) },
    );
    // The voice actions run the same handlers as their keys (the open conversation's header and composer attach them).
    if (open && isShortcutAvailable("listen.mode")) commands.push({ id: "listen.mode", label: "Listen mode", description: "Speak new messages from Silicons in this conversation", group: "This conversation", icon: icon(Headphones), shortcutId: "listen.mode", keywords: ["voice", "speak", "hear", "headphones"], run: () => void runShortcut("listen.mode") });
    if (open && !readOnly && isShortcutAvailable("composer.record")) commands.push({ id: "composer.record", label: "Record a voice message", group: "This conversation", icon: icon(Mic), shortcutId: "composer.record", keywords: ["voice", "microphone", "audio"], run: () => void runShortcut("composer.record") });
    if (open && !readOnly && isShortcutAvailable("composer.dictate")) commands.push({ id: "composer.dictate", label: "Dictate", description: "Speak, and the words go in the message field", group: "This conversation", icon: icon(AudioLines), shortcutId: "composer.dictate", keywords: ["voice", "speech to text", "transcribe"], run: () => void runShortcut("composer.dictate") });
    for (const silicon of me.custodian_of) {
      if (view === silicon.id) continue;
      commands.push({ id: `view.${silicon.id}`, label: `View as ${silicon.id}`, description: `${accountName(silicon)}'s conversations, read only`, group: "Viewing as", icon: icon(Eye), keywords: ["custodian", "silicon", silicon.display_name ?? ""], run: () => go(inboxHref(silicon.id)) });
    }
    if (view) commands.push({ id: "view.mine", label: "Back to your conversations", group: "Viewing as", icon: icon(Eye), keywords: ["mine", "own"], run: () => go(inboxHref(null)) });
    for (const conversation of items) {
      const title = conversationTitle(conversation, perspective);
      commands.push({
        id: `open.${conversation.id}`,
        label: title,
        description: `${conversationKindWords(conversation, perspective)} · ${conversation.id}`,
        group: "Conversations",
        icon: icon(conversation.group ? Users : MessageCircle),
        keywords: [conversation.id, ...conversation.participants.flatMap(account => [account.id, account.display_name ?? ""]), conversation.group?.name ?? ""],
        run: () => go(conversationHref(conversation.id, { view })),
      });
    }
    return commands;
  }, [items, open, view, me.custodian_of, perspective, details.open, details.toggle, go, accountsUrl, signOut, readOnly, available]);

  return null;
}
