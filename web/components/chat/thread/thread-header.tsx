"use client";

/**
 * The thread's header (spec §6.3): back to the list (phones), the face, the name (the page's h1), the kind in words,
 * and a status line (online, last seen…, typing…, recording a voice message…, N members of a group, N people in a
 * conversation of several, or who is busy; "waiting for the network" while this browser is offline, when nobody's
 * presence is known), then Listen mode, the details panel (ⓘ, ⌘.) and a menu of the rest (copy
 * the address, the link, the CLI command; mark read). What someone starts doing is announced politely; presence coming
 * and going is not.
 */
import Link from "next/link";
import { ArrowLeft, CheckCheck, Copy, Ellipsis, Info, Link2, Terminal } from "lucide-react";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { Tooltip } from "@/components/silicon-ui/tooltip/tooltip";
import { conversationHref, inboxHref, isGroupAddress, kindWord, shellQuote, type ConversationKind } from "@/lib/chat/address";
import { lastSeen } from "@/lib/chat/format";
import type { AccountRef, ConversationSummary, Presence } from "@/lib/chat/types";
import { ConversationAvatar } from "../common/avatars";
import { IconAction } from "../common/icon-action";
import { useDetails } from "../shell/details";
import { ListenModeToggle } from "../voice/voice-parts";
import { activityStatus, busyWords, peopleWords, type Busy } from "./thread-model";
import styles from "./thread.module.css";

export interface ThreadHeaderProps {
  address: string;
  view: string | null;
  title: string;
  kind: ConversationKind;
  conversation: ConversationSummary | undefined;
  others: AccountRef[];
  presence: Map<string, Presence>;
  /** Who is busy right now (typing, recording…). */
  busy: Busy[];
  /** This browser is offline: nobody's presence is known until it is back. */
  offline?: boolean;
  onCopy: (text: string, what: string) => void;
  onMarkRead: (() => void) | null;
}

/**
 * What the line under the name says, and whether it is someone doing something (shown in the accent, announced). A
 * group counts its members; a conversation of several without a name counts its people (as the details panel lists
 * them).
 */
function statusLine(kind: ConversationKind, several: boolean, others: AccountRef[], presence: Map<string, Presence>, busy: Busy[], conversation: ConversationSummary | undefined, offline: boolean): { text: string; extra?: string; live: boolean; online: boolean; alone?: boolean } {
  if (busy.length) return { text: kind === "group" || others.length !== 1 ? busyWords(busy) : activityStatus(busy[0]!.activity), live: true, online: true };
  if (kind === "group" || others.length !== 1) {
    const count = conversation?.participants.length ?? others.length + 1;
    const here = others.filter(account => presence.get(account.id)?.availability === "online").length;
    return { text: several ? peopleWords(count) : `${count} ${count === 1 ? "member" : "members"}`, extra: here ? `, ${here} online` : undefined, live: false, online: false };
  }
  // What this browser last heard is no longer true once it is offline: the line says what it waits for instead, alone
  // (the kind and the id are in the title row and the thread's beginning), so a phone has room for it.
  if (offline) return { text: "waiting for the network", live: false, online: false, alone: true };
  const state = presence.get(others[0]!.id);
  if (!state) return { text: "", live: false, online: false };
  if (state.availability === "online") return { text: "online", live: false, online: true };
  // Never seen on a device (a Silicon on the CLI, say): nothing to say rather than a bare "offline".
  return { text: state.last_seen_at ? lastSeen(state.last_seen_at) : "", live: false, online: false };
}

export function ThreadHeader({ address, view, title, kind, conversation, others, presence, busy, offline = false, onCopy, onMarkRead }: ThreadHeaderProps) {
  const details = useDetails();
  // A conversation of several without a name is not a group (no name, no owners): its line says how many people.
  const several = kind === "group" && !conversation?.group && !isGroupAddress(address);
  const status = statusLine(kind, several, others, presence, busy, conversation, offline);
  const single = kind !== "group" && others.length === 1 ? others[0]! : null;
  const link = typeof window === "undefined" ? conversationHref(address, { view }) : `${window.location.origin}${conversationHref(address, { view })}`;
  // A custodian's view is read only: its command reads the conversation (a custodian may), it does not send as the Silicon.
  const cli = view ? `dm messages list ${shellQuote(address)}` : `dm messages send ${shellQuote(address)} --text 'hello'`;
  return (
    <header className={styles.header}>
      <Link href={inboxHref(view)} className={styles.back} data-sq="surface" aria-label="Back to conversations">
        <ArrowLeft size={20} strokeWidth={1.75} aria-hidden="true" />
      </Link>
      <ConversationAvatar group={conversation?.group} others={others} size="header" presence={single ? presence.get(single.id) : undefined} decorative />
      <div className={styles.headerText}>
        <div className={styles.titleRow}>
          <h1 className={styles.title} id="thread-title">{title}</h1>
          {several ? null : <span className={styles.kind} data-kind={kind} data-sq="surface">{kindWord(kind)}</span>}
        </div>
        <p className={styles.status} data-live={status.live ? "" : undefined} data-online={status.online ? "" : undefined}>
          {/* On a phone the kind chip gives way to the title: the line says the kind instead of the id. */}
          {several || status.alone ? null : <span className={styles.statusKind}>{kindWord(kind)}</span>}
          {single && single.id !== title && !status.alone ? <span className={styles.statusId}>{single.id}</span> : null}
          {status.text ? <span className={styles.statusText}>{status.text}{status.extra ? <span className={styles.statusExtra}>{status.extra}</span> : null}</span> : null}
        </p>
        <span className="sr-only" role="status">{status.live ? `${title}: ${status.text}` : ""}</span>
      </div>
      <div className={styles.headerActions}>
        <ListenModeToggle address={address} />
        <IconAction label={details.open ? "Hide details" : "Show details"} shortcut="details.toggle" icon={<Info strokeWidth={1.75} aria-hidden="true" />} pressed={details.open} onClick={details.toggle} />
        <DropdownMenuRoot>
          <Tooltip content="More" side="bottom">
            <DropdownMenuTrigger asChild>
              <button type="button" className={styles.headerMore} data-sq="surface" aria-label="More for this conversation">
                <Ellipsis size={18} strokeWidth={1.75} aria-hidden="true" />
              </button>
            </DropdownMenuTrigger>
          </Tooltip>
          <DropdownMenuContent align="end">
            <DropdownMenuItem icon={<Copy size={16} strokeWidth={1.75} />} onSelect={() => onCopy(address, "Address")}>Copy the address</DropdownMenuItem>
            <DropdownMenuItem icon={<Link2 size={16} strokeWidth={1.75} />} onSelect={() => onCopy(link, "Link")}>Copy the link</DropdownMenuItem>
            <DropdownMenuItem icon={<Terminal size={16} strokeWidth={1.75} />} onSelect={() => onCopy(cli, "CLI command")}>Copy the CLI command</DropdownMenuItem>
            {onMarkRead ? (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem icon={<CheckCheck size={16} strokeWidth={1.75} />} onSelect={onMarkRead}>Mark as read</DropdownMenuItem>
              </>
            ) : null}
          </DropdownMenuContent>
        </DropdownMenuRoot>
      </div>
    </header>
  );
}
