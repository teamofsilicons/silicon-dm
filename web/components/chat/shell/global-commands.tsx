"use client";

/**
 * The shortcuts that work anywhere (spec §6.7) and the command palette's entries (spec §6.7: conversations, actions,
 * custodian views, each with its keys). Both run the same actions, so ⌘K finds everything the keyboard can do.
 */
import { useRouter } from "next/navigation";
import { useCallback, useEffect } from "react";
import { AudioLines, BookOpen, CircleUserRound, Copy, Eye, Headphones, Inbox, Keyboard, LogOut, Mail, MessageCircle, MessageSquarePlus, Mic, PanelRight, Settings2, Terminal, Users, Volume2 } from "lucide-react";
import { appConfig } from "@/lib/app.config";
import { accountName, conversationHref, conversationKindWords, conversationTitle, inboxHref, otherParticipants, sameAccount, shellQuote } from "@/lib/chat/address";
import { unreadOf } from "@/lib/chat/cache";
import { loadConversationsUntil, useConversations, useLoadEverything } from "@/lib/chat/hooks";
import { markAllRead } from "@/lib/chat/receipts";
import { useSignOut } from "@/lib/client/session";
import { toggleCommandPalette, useCommandPaletteQuery, useRegisterCommands, type ShellCommand } from "@/lib/commands";
import { notify } from "@/lib/notify";
import { isShortcutAvailable, runShortcut, setShortcutAvailability, useShortcut, useShortcutAvailability } from "@/lib/shortcuts/react";
import { useVoiceAvailability } from "@/lib/voice";
import { askToSignOut } from "../dialogs/sign-out-dialog";
import { openDialog } from "../dialogs/store";
import { requestSearchFocus, useOpenAddress } from "../sidebar/conversations-pane";
import type { ConversationSummary } from "@/lib/chat/types";
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
  const list = useConversations(view);
  const { items } = list;
  const openAddress = useOpenAddress();
  const details = useDetails();
  const { signOut } = useSignOut();
  // Voice actions appear in the palette once they work here (deps below re-register then).
  useVoiceShortcutAvailability();
  const available = useShortcutAvailability();
  // A query typed in the palette looks through every conversation, not only the first ones loaded.
  const paletteQuery = useCommandPaletteQuery();
  useLoadEverything(view, !!paletteQuery.trim());

  const go = useCallback((href: string) => router.push(href), [router]);

  /**
   * The next or previous (unread) conversation from the open one, in list order. Forward, the conversations further down
   * the list are loaded before it starts again from the top; back to the very end for an unread one, all of them are.
   */
  const step = useCallback(async (direction: 1 | -1, unreadOnly: boolean) => {
    const fits = (candidate: ConversationSummary) => candidate.id !== openAddress && (!unreadOnly || unreadOf(candidate, perspective) > 0);
    const at = (rows: ConversationSummary[]) => rows.findIndex(item => item.id === openAddress);
    let rows = items;
    if (direction === 1 && !rows.slice(at(rows) + 1).some(fits)) rows = await loadConversationsUntil(list, loaded => loaded.slice(at(loaded) + 1).some(fits));
    if (direction === -1 && unreadOnly && !rows.slice(0, Math.max(at(rows), 0)).some(fits)) rows = await loadConversationsUntil(list, () => false);
    const i = at(rows);
    const order = direction === 1 ? [...rows.slice(i + 1), ...rows.slice(0, Math.max(i, 0))] : [...rows.slice(0, Math.max(i, 0)).reverse(), ...rows.slice(i + 1).reverse()];
    const next = order.find(fits);
    if (next) go(conversationHref(next.id, { view }));
    else if (unreadOnly) notify.info("Nothing unread", "Every conversation is read.");
  }, [items, list, openAddress, perspective, view, go]);

  /** Every conversation read, the ones further down the list too (they are loaded first). */
  const markEverythingRead = useCallback(async () => {
    const rows = await loadConversationsUntil(list, () => false);
    const count = await markAllRead(rows.filter(item => unreadOf(item, perspective) > 0));
    if (count) notify.success("Everything is read", count === 1 ? "1 conversation marked read." : `${count} conversations marked read.`);
  }, [list, perspective]);

  useShortcut("palette.open", () => toggleCommandPalette());
  // On a phone (or a narrow window) the list is its own page: '/' goes there and puts the keyboard in its search.
  useShortcut("search.focus", () => {
    if (focusConversationSearch()) return;
    requestSearchFocus();
    go(inboxHref(view));
  });
  useShortcut("shortcuts.open", () => openDialog("shortcuts"));
  useShortcut("conversation.new", () => openDialog("new-conversation"), !readOnly);
  useShortcut("group.new", () => openDialog("new-group"), !readOnly);
  useShortcut("conversation.previous", () => void step(-1, false));
  useShortcut("conversation.next", () => void step(1, false));
  useShortcut("conversation.previousUnread", () => void step(-1, true));
  useShortcut("conversation.nextUnread", () => void step(1, true));
  useShortcut("conversations.markAllRead", () => void markEverythingRead(), !readOnly);
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
      { id: "conversations.markAllRead", label: "Mark everything read", group: "Actions", icon: icon(Inbox), shortcutId: "conversations.markAllRead", keywords: ["clear", "unread"], run: () => void markEverythingRead() },
      { id: "shortcuts.open", label: "Keyboard shortcuts", group: "Actions", icon: icon(Keyboard), shortcutId: "shortcuts.open", keywords: ["keys", "cheatsheet", "help"], run: () => openDialog("shortcuts") },
      { id: "go.settings", label: "Settings", group: "Go to", icon: icon(Settings2), shortcutId: "go.settings", keywords: ["preferences", "sending", "notifications"], run: () => go("/settings") },
      { id: "go.voice", label: "Voice settings", group: "Go to", icon: icon(Volume2), shortcutId: "go.voice", keywords: ["voices", "speak", "listening"], run: () => go("/settings#voice") },
      { id: "go.keyboard", label: "Change keyboard shortcuts", group: "Go to", icon: icon(Keyboard), keywords: ["rebind", "keys"], run: () => go("/settings#keyboard") },
      { id: "go.docs", label: `${appConfig.name} docs`, description: "Opens in a new tab", group: "Go to", icon: icon(BookOpen), keywords: ["help", "guide", "cli"], run: () => void window.open(appConfig.links.docs, "_blank", "noopener") },
      { id: "go.account", label: "Your account", description: "Silicon Accounts, in a new tab", group: "Go to", icon: icon(CircleUserRound), keywords: ["profile", "photo"], run: () => void window.open(accountsUrl, "_blank", "noopener") },
      { id: "account.signout", label: "Sign out", description: `Sign this browser out of ${appConfig.name}`, group: "Account", icon: icon(LogOut), keywords: ["log out", "logout"], run: () => void askToSignOut(signOut) },
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
      const others = otherParticipants(conversation, perspective).filter(account => !sameAccount(account, perspective));
      commands.push({
        id: `open.${conversation.id}`,
        label: title,
        description: `${conversationKindWords(conversation, perspective)} · ${conversation.id}`,
        group: "Conversations",
        icon: icon(conversation.group ? Users : MessageCircle),
        // Found by its name, its address and its people's ids and names (yours left out: you are in every one), ahead of
        // an action that merely contains the letters (lib/command-rank.ts). A direct conversation answers to the other
        // account's id as to its name.
        conversation: true,
        names: others.length === 1 ? [conversation.id, others[0]!.id] : [conversation.id],
        members: others.length === 1 ? [] : others.flatMap(account => [account.id, account.display_name ?? ""]),
        run: () => go(conversationHref(conversation.id, { view })),
      });
    }
    return commands;
  }, [items, open, view, me.custodian_of, perspective, details.open, details.toggle, go, accountsUrl, signOut, readOnly, available, markEverythingRead]);

  return null;
}
