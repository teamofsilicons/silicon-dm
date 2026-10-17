"use client";

/**
 * People in the details panel. `Profile` is the one other account of a direct conversation: face, name, id (with
 * copy), Carbon or Silicon, presence, and "Message". `PersonRow` is one person in a list (a group's members, its
 * invitations, a conversation of several), with who looks after it when it is a Silicon, and an actions menu for what
 * can be done to them; a removal asks in place (Silicon UI's ConfirmMorph) where the menu was.
 */
import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Copy, Ellipsis, MessageCircle } from "lucide-react";
import { ConfirmMorph, type ConfirmMorphState } from "@/components/silicon-ui/confirm-morph/confirm-morph";
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { KindMarker } from "@/components/foundation/account/account-chip";
import { accountName, firstName, sameAccount } from "@/lib/chat/address";
import { lastSeen } from "@/lib/chat/format";
import { activityWords } from "@/lib/chat/presence";
import type { AccountRef, Presence } from "@/lib/chat/types";
import { notify, notifyError } from "@/lib/notify";
import { AccountAvatar } from "../common/avatars";
import { useStartConversation } from "../dialogs/open-conversation";
import { useChat } from "../shell/chat-context";
import { useDetails } from "../shell/details";
import styles from "./details.module.css";

/** Copies text and says so, or says why the browser did not let it. */
export async function copyText(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    notify.success(`${what} copied`, text);
  } catch {
    notify.error({ status: 0, code: "clipboard_refused", message: "This browser did not let the page copy.", hint: "Select the text and copy it yourself." }, `Could not copy the ${what.toLowerCase()}`);
  }
}

/**
 * What anyone can do to a person listed in the panel: message them (not yourself, and not in a custodian's read-only
 * view) and copy their id. A group's owners add more (group-details.tsx).
 */
export function usePersonBasics(): (account: AccountRef) => PersonAction[] {
  const { perspective, readOnly } = useChat();
  const details = useDetails();
  const start = useStartConversation();
  return useCallback((account: AccountRef) => {
    const list: PersonAction[] = [];
    if (!sameAccount(account, perspective) && !readOnly && !account.deleted) {
      list.push({
        label: `Message ${firstName(account)}`,
        icon: <MessageCircle size={16} strokeWidth={1.75} />,
        onSelect: () => {
          if (details.mode !== "wide") details.setOpen(false);
          start([account.id]).catch(failure => notifyError(failure, `Could not open a conversation with ${firstName(account)}`));
        },
      });
    }
    list.push({ label: "Copy id", icon: <Copy size={16} strokeWidth={1.75} />, onSelect: () => void copyText(account.id, "Id") });
    return list;
  }, [details, perspective, readOnly, start]);
}

/** What someone is doing or when they were last here, as the header says it; null when DM does not know. */
export function presenceWords(presence: Presence | undefined): { text: string; online: boolean } | null {
  if (!presence) return null;
  if (presence.activity) return { text: activityWords(presence.activity), online: true };
  return presence.availability === "online" ? { text: "online", online: true } : { text: lastSeen(presence.last_seen_at), online: false };
}

function PresenceLine({ presence }: { presence: Presence | undefined }) {
  const words = presenceWords(presence);
  if (!words) return null;
  return <span className={styles.presence} data-online={words.online ? "" : undefined}>{words.text}</span>;
}

/** The one other account of a direct conversation. */
export function Profile({ account, presence, children }: { account: AccountRef; presence?: Presence; children?: ReactNode }) {
  return (
    <section className={styles.profile} aria-labelledby="details-profile-name">
      <AccountAvatar account={account} size="header" presence={presence} decorative className={styles.profileFace} />
      <div className={styles.profileText}>
        <h3 id="details-profile-name" className={styles.profileName}>{accountName(account)}</h3>
        <p className={styles.profileMeta}>
          <span className={styles.idWithCopy}>
            <span className={styles.mono}>{account.id}</span>
            <CopyButton value={account.id} label={`Copy ${account.id}`} iconOnly variant="plain" />
          </span>
          <KindMarker kind={account.type} size="xs" />
        </p>
        <PresenceLine presence={presence} />
      </div>
      {children ? <div className={styles.profileActions}>{children}</div> : null}
    </section>
  );
}

export interface PersonAction {
  label: string;
  icon?: ReactNode;
  onSelect: () => void;
  destructive?: boolean;
  /** A separator goes before it. */
  separated?: boolean;
}

export interface PersonRemoval {
  /** The question, such as "Remove Bo from the group?". */
  prompt: string;
  confirmLabel: string;
  pendingLabel: string;
  doneLabel: string;
  run: () => Promise<unknown>;
}

/** One person in a list, with presence, notes (Owner, Invited, You), a Silicon's custodian and a menu of actions. */
export function PersonRow({ account, presence, notes = [], custodian = null, actions = [], removal, removing, onRemovingChange }: {
  account: AccountRef;
  presence?: Presence;
  notes?: string[];
  /** For a Silicon: who looks after it, as lib/chat/address.ts custodianWords says it. */
  custodian?: string | null;
  actions?: PersonAction[];
  /** A removal that asks in place (it replaces the menu while it asks). */
  removal?: PersonRemoval;
  removing?: boolean;
  onRemovingChange?: (removing: boolean) => void;
}) {
  const menuButton = useRef<HTMLButtonElement>(null);
  const morph = useRef<HTMLDivElement>(null);
  const [asking, setAsking] = useState(false);
  const wasRemoving = useRef(false);
  const name = accountName(account);

  // The question takes the keyboard when it appears (from the menu); when it goes, focus returns to the menu button.
  useEffect(() => {
    if (removing && !wasRemoving.current) {
      const frame = requestAnimationFrame(() => morph.current?.querySelector<HTMLElement>("button[data-autofocus]")?.focus());
      wasRemoving.current = true;
      return () => cancelAnimationFrame(frame);
    }
    if (!removing && wasRemoving.current) {
      wasRemoving.current = false;
      if (!menuButton.current?.isConnected || document.activeElement === document.body) requestAnimationFrame(() => menuButton.current?.focus());
    }
  }, [removing]);

  return (
    <li className={styles.person} data-account={account.id} data-asking={removing && removal ? "" : undefined}>
      <AccountAvatar account={account} size="header" presence={presence} decorative />
      <span className={styles.personText}>
        <span className={styles.personName}>{name}</span>
        <span className={styles.personMeta}>
          {name !== account.id ? <span className={styles.mono}>{account.id}</span> : null}
          <KindMarker kind={account.type} size="xs" />
          {notes.map(note => <span key={note} className={styles.note}>{note}</span>)}
        </span>
        {custodian ? <span className={styles.custodian}>{custodian}</span> : null}
        <PresenceLine presence={presence} />
      </span>
      {removing && removal ? (
        <ConfirmMorph ref={morph} className={styles.morph} label={removal.confirmLabel} prompt={removal.prompt} confirmLabel={removal.confirmLabel} pendingLabel={removal.pendingLabel} doneLabel={removal.doneLabel}
          defaultState="confirming" onConfirm={removal.run}
          onStateChange={(state: ConfirmMorphState) => { if (state === "idle") onRemovingChange?.(false); }} />
      ) : actions.length ? (
        <DropdownMenuRoot onOpenChange={setAsking}>
          <DropdownMenuTrigger asChild>
            <button ref={menuButton} type="button" className={styles.rowMenu} data-sq="surface" aria-label={`Actions for ${name}`} data-state={asking ? "open" : undefined}>
              <Ellipsis size={16} strokeWidth={1.75} aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {actions.map(action => (
              <Fragment key={action.label}>
                {action.separated ? <DropdownMenuSeparator /> : null}
                <DropdownMenuItem icon={action.icon} destructive={action.destructive} onSelect={action.onSelect}>{action.label}</DropdownMenuItem>
              </Fragment>
            ))}
          </DropdownMenuContent>
        </DropdownMenuRoot>
      ) : null}
    </li>
  );
}

/** "Bo" for menus and questions: a first name, or the handle. */
export const shortName = (account: AccountRef) => firstName(account);
