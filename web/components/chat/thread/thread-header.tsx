"use client";

/**
 * The thread's header (spec §6.3): back to the list (phones), the face, the name (the page's h1), the kind in words,
 * and a status line (online, last seen, typing…, recording a voice message…, N members), then Listen mode, the details
 * panel (ⓘ, ⌘.) and a menu of the rest (copy the address, the link, the CLI command; mark read).
 */
import Link from "next/link";
import { ArrowLeft, Ellipsis, Info, Terminal, Copy, Link2, CheckCheck } from "lucide-react";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { Tooltip } from "@/components/silicon-ui/tooltip/tooltip";
import { conversationHref, firstName, inboxHref, kindWord, shellQuote, type ConversationKind } from "@/lib/chat/address";
import { lastSeen } from "@/lib/chat/format";
import { activityWords } from "@/lib/chat/presence";
import type { AccountRef, ConversationSummary, Presence } from "@/lib/chat/types";
import { ConversationAvatar } from "../common/avatars";
import { IconAction } from "../common/icon-action";
import { useDetails } from "../shell/details";
import { ListenModeToggle } from "../voice/voice-parts";
import styles from "./thread.module.css";

export interface ThreadHeaderProps {
  address: string;
  view: string | null;
  title: string;
  kind: ConversationKind;
  conversation: ConversationSummary | undefined;
  others: AccountRef[];
  presence: Map<string, Presence>;
  onCopy: (text: string, what: string) => void;
  onMarkRead: (() => void) | null;
}

/** What the line under the name says. */
function statusLine(kind: ConversationKind, others: AccountRef[], presence: Map<string, Presence>, conversation: ConversationSummary | undefined): { text: string; live: boolean } {
  const active = others.map(account => ({ account, presence: presence.get(account.id) })).find(entry => entry.presence?.activity);
  if (active?.presence?.activity) {
    const words = activityWords(active.presence.activity);
    return { text: kind === "group" ? `${firstName(active.account)} is ${words}` : words, live: true };
  }
  if (kind === "group" || others.length !== 1) {
    const count = conversation?.participants.length ?? others.length + 1;
    return { text: `${count} ${count === 1 ? "member" : "members"}`, live: false };
  }
  const state = presence.get(others[0]!.id);
  if (!state) return { text: kindWord(kind), live: false };
  return { text: state.availability === "online" ? "online" : lastSeen(state.last_seen_at), live: false };
}

export function ThreadHeader({ address, view, title, kind, conversation, others, presence, onCopy, onMarkRead }: ThreadHeaderProps) {
  const details = useDetails();
  const status = statusLine(kind, others, presence, conversation);
  const single = kind !== "group" && others.length === 1 ? others[0]! : null;
  const link = typeof window === "undefined" ? conversationHref(address, { view }) : `${window.location.origin}${conversationHref(address, { view })}`;
  const cli = `dm messages send ${shellQuote(address)} --text 'hello'`;
  return (
    <header className={styles.header}>
      <Link href={inboxHref(view)} className={styles.back} data-sq="surface" aria-label="Back to conversations">
        <ArrowLeft size={20} strokeWidth={1.75} aria-hidden="true" />
      </Link>
      <ConversationAvatar group={conversation?.group} others={others} size="header" presence={single ? presence.get(single.id) : undefined} decorative />
      <div className={styles.headerText}>
        <div className={styles.titleRow}>
          <h1 className={styles.title} id="thread-title">{title}</h1>
          <span className={styles.kind} data-kind={kind} data-sq="surface">{kindWord(kind)}</span>
        </div>
        <p className={styles.status} data-live={status.live ? "" : undefined} aria-live="polite">
          {single && single.id !== title ? <span className={styles.statusId}>{single.id}</span> : null}
          {single && single.id !== title ? <span className={styles.statusDot} aria-hidden="true">·</span> : null}
          <span>{status.text}</span>
        </p>
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
