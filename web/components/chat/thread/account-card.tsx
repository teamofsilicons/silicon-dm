"use client";

/**
 * Account cards (spec §6.4): in a group, a sender's name and face open a card on hover or keyboard focus (Silicon UI's
 * hover card): the name, Carbon or Silicon in words, presence, the id with a copy button, a Silicon's custodian, and
 * "Message" (your own conversation with them).
 *
 * The card describes its trigger for assistive technology (it is the trigger's description) and its buttons serve the
 * pointer. The keyboard acts on the trigger itself, as Silicon UI asks of hover cards: Enter or Space opens the
 * conversation and C copies the id, and a card opened by keyboard focus says so.
 */
import { useState, type KeyboardEvent, type ReactNode } from "react";
import { HoverCard } from "@/components/silicon-ui/hover-card/hover-card";
import { Button } from "@/components/silicon-ui/button/button";
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
import { Kbd } from "@/components/silicon-ui/shortcut-recorder/shortcut-recorder";
import { accountName, firstName } from "@/lib/chat/address";
import { lastSeen } from "@/lib/chat/format";
import type { AccountRef, Presence } from "@/lib/chat/types";
import { AccountAvatar } from "../common/avatars";
import { activityStatus } from "./thread-model";
import { useThreadActions, useThreadPeople } from "./thread-actions";
import styles from "./thread.module.css";

function presenceWords(presence: Presence | undefined): string | null {
  if (!presence) return null;
  if (presence.activity) return activityStatus(presence.activity);
  if (presence.availability === "online") return "online";
  return presence.last_seen_at ? lastSeen(presence.last_seen_at) : null;
}

function AccountCard({ account, keyboard }: { account: AccountRef; keyboard: boolean }) {
  const actions = useThreadActions();
  const people = useThreadPeople();
  const presence = people.presence.get(account.id);
  const words = presenceWords(presence);
  const custodian = people.custodianOf(account);
  const self = actions.isSelf(account);
  const kind = account.type === "silicon" ? "Silicon" : "Carbon";
  return (
    <div className={styles.card}>
      <div className={styles.cardHead}>
        <AccountAvatar account={account} size="row" presence={presence} decorative />
        <div className={styles.cardIdentity}>
          <p className={styles.cardName}>{accountName(account)}{self ? <span className={styles.cardYou}> (you)</span> : null}</p>
          <p className={styles.cardKind}>
            <span data-kind={account.type}>{kind}</span>
            {words ? <span data-online={presence?.availability === "online" || presence?.activity ? "" : undefined}>{words}</span> : null}
          </p>
        </div>
      </div>
      <div className={styles.cardId} data-sq="surface">
        <code className={styles.cardIdValue}>{account.id}</code>
        <CopyButton value={account.id} label={`Copy ${account.id}`} iconOnly variant="plain" />
      </div>
      {custodian ? <p className={styles.cardLine}>Looked after by {actions.isSelf(custodian) ? "you" : accountName(custodian)}</p> : null}
      {account.deleted ? <p className={styles.cardLine}>This account was deleted.</p> : self ? null : (
        <div className={styles.cardActions}>
          <Button size="sm" variant="secondary" onClick={() => actions.openDirect(account)}>Message {firstName(account)}</Button>
          {keyboard ? (
            <span className={styles.cardHint}>
              <Kbd size="sm">Enter</Kbd> message <span aria-hidden="true">·</span> <Kbd size="sm">C</Kbd> copy the id
            </span>
          ) : null}
        </div>
      )}
    </div>
  );
}

export interface AccountTriggerProps {
  account: AccountRef;
  children: ReactNode;
  className?: string;
  /** Out of the Tab order for good (a face beside its name: the name is the one stop). */
  untabbable?: boolean;
  /** The name a person would say, when the trigger shows only a face. */
  label?: string;
}

export function AccountTrigger({ account, children, className, untabbable, label }: AccountTriggerProps) {
  const actions = useThreadActions();
  const [keyboard, setKeyboard] = useState(false);
  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (!actions.isSelf(account) && !account.deleted) actions.openDirect(account);
    } else if ((event.key === "c" || event.key === "C") && !event.shiftKey) {
      event.preventDefault();
      actions.copyId(account);
    }
  };
  return (
    <HoverCard content={<AccountCard account={account} keyboard={keyboard} />} side="top" align="start">
      <button
        type="button"
        className={className}
        tabIndex={untabbable ? -1 : undefined}
        aria-label={label}
        onKeyDown={onKeyDown}
        onFocus={event => setKeyboard(event.currentTarget.matches(":focus-visible"))}
        onPointerDown={() => setKeyboard(false)}
      >
        {children}
      </button>
    </HoverCard>
  );
}
