"use client";

/**
 * A group in the details panel: its name and description (an owner renames it in place with Silicon UI's InlineEdit;
 * its address never changes), its members with presence and roles, adding members (the account picker; a refusal is
 * the service's words), the invitations still open, and leaving (held, so a stray tap never leaves). Each member's
 * menu has Message, Copy id and, for owners, Make owner / Remove as owner and Remove from group, which asks in place;
 * a custodian who is not an owner may still take its own Silicons out.
 *
 * A custodian's "Viewing as" view shows all of it read only. While the panel shows, the group is asked for again every
 * 30 s, and what changed reaches the thread's header and the conversation list too.
 */
import { useRouter } from "next/navigation";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Copy, Crown, UserMinus, UserPlus } from "lucide-react";
import { Button } from "@/components/silicon-ui/button/button";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import { InlineEdit } from "@/components/silicon-ui/inline-edit/inline-edit";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { accountName, custodianWords, firstName, inboxHref, sameAccount } from "@/lib/chat/address";
import { useGroup } from "@/lib/chat/hooks";
import { useParticipantsPresence } from "@/lib/chat/presence";
import type { AccountRef, ConversationSummary, GroupDetails as Group, Presence } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { notify, notifyError } from "@/lib/notify";
import { AccountPicker, type AccountPickerHandle } from "../dialogs/account-picker";
import { idsNamedIn, isListed, keySet } from "../dialogs/contacts";
import { useShowWhenSet } from "../dialogs/show-when-set";
import { useKnownAccounts } from "../dialogs/use-known-accounts";
import { useChat } from "../shell/chat-context";
import { useDetails } from "../shell/details";
import { applyGroupToSummaries, summaryDiffers, useGroupActions } from "./group-actions";
import { copyText, PersonRow, usePersonBasics, type PersonAction } from "./person";
import styles from "./details.module.css";

/** How often an open panel asks for the group again (members accept, leave, are renamed by another owner). */
const REFRESH_MS = 30_000;
/** How many members' presence an open panel follows. */
const PRESENCE_LIMIT = 24;

/** A group's name is one line: line breaks and runs of spaces become one space. */
export const groupNameOf = (text: string) => text.replace(/\s+/g, " ").trim();

/**
 * The name field wraps like the title it replaces (a long name is never cut off), but a name has no line breaks:
 * Shift+Enter, which adds one in a multiline InlineEdit, does nothing here (Enter saves, as everywhere).
 */
function noLineBreaks(event: KeyboardEvent<HTMLElement>) {
  if (event.key === "Enter" && event.shiftKey && event.target instanceof HTMLTextAreaElement) event.preventDefault();
}

function AddMembers({ inGroup, onAdd, onDone }: { inGroup: AccountRef[]; onAdd: (ids: string[]) => Promise<Group>; onDone: () => void }) {
  const known = useKnownAccounts();
  const picker = useRef<AccountPickerHandle>(null);
  const [accounts, setAccounts] = useState<AccountRef[]>([]);
  const [error, setError] = useState<ApiError | null>(null);
  const [busy, setBusy] = useState(false);
  const errorBox = useShowWhenSet<HTMLDivElement>(error);
  // "Add members" is gone once the form shows: the keyboard goes straight to the field (it would fall to the page).
  useEffect(() => picker.current?.focus(), []);
  const submit = async () => {
    if (busy) return;
    const { accounts: chosen, problem } = picker.current?.commit() ?? { accounts, problem: null };
    if (problem) return;
    if (!chosen.length) {
      picker.current?.focus();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const next = await onAdd(chosen.map(account => account.id));
      const joined = chosen.filter(account => isListed(account, keySet(next.members)));
      notify.success(chosen.length === 1 ? `${joined.length ? "Added" : "Invited"} ${accountName(chosen[0]!)}` : `Invited ${chosen.length} accounts`, joined.length ? "Your own Silicons join at once." : "They join when they accept.");
      onDone();
    } catch (failure) {
      setError(ApiError.from(failure));
      setBusy(false);
    }
  };
  return (
    // data-editing: inside the drawer or the bottom sheet, Esc closes this form first, not the whole layer (lib/escape.ts).
    // Esc that the field uses itself (clearing typed text, closing its list) never reaches the form.
    <form className={styles.addForm} data-editing="" aria-label="Add members" onSubmit={event => { event.preventDefault(); void submit(); }}
      onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onDone(); } }}>
      <AccountPicker ref={picker} label="Invite" value={accounts} onChange={next => { setAccounts(next); setError(null); }} suggestions={known} exclude={inGroup}
        excludedReason={account => `${account.id} is already in the group or invited.`} listSize={4} showRecent={false}
        flagged={error ? idsNamedIn(`${error.message} ${error.hint ?? ""}`, accounts) : []}
        description="They join when they accept; your own Silicons join at once." />
      {error ? <div ref={errorBox}><ErrorAlert error={error} title="Not invited" /></div> : null}
      <div className={styles.formActions}>
        <Button type="button" size="sm" variant="ghost" onClick={onDone}>Cancel</Button>
        <Button type="submit" size="sm" loading={busy}>Invite</Button>
      </div>
    </form>
  );
}

export function GroupDetailsPanel({ address, conversation, presence }: { address: string; conversation: ConversationSummary | undefined; presence: Map<string, Presence> }) {
  const { me, view, perspective, readOnly } = useChat();
  const router = useRouter();
  const details = useDetails();
  const query = useGroup(address, view);
  const group: Group | undefined = query.data?.group;
  const summary = group ?? (conversation?.group as Group | undefined);
  const actions = useGroupActions(address, view, group);
  const basics = usePersonBasics();
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);
  const [renameError, setRenameError] = useState<ApiError | null>(null);
  const [leaveError, setLeaveError] = useState<ApiError | null>(null);
  const [left, setLeft] = useState(false);
  const addButton = useRef<HTMLButtonElement>(null);

  const members = useMemo(() => {
    const list = group?.members ?? conversation?.participants ?? [];
    const owners = keySet(summary?.owners ?? []);
    // Owners first, then everyone else, each in the service's order.
    return [...list.filter(member => isListed(member, owners)), ...list.filter(member => !isListed(member, owners))];
  }, [group, conversation, summary]);
  const owners = useMemo(() => keySet(summary?.owners ?? []), [summary]);
  const amOwner = isListed(perspective, owners);
  // The conversation (already in the cache when the panel opens) says who the owners are, so an owner's controls are
  // there at once instead of appearing when the group's full details arrive; a change needs no version but a rename,
  // and that one fetches it (group-actions.ts).
  const canManage = !readOnly && amOwner;
  const inGroup = useMemo(() => [...members, ...(group?.pending_invitations ?? [])], [members, group]);
  // Presence is one request per account every 15 s while the panel shows, so a big group asks for the first 24 people
  // listed (owners first) rather than everyone.
  const others = useMemo(() => members.filter(member => !sameAccount(member, perspective)).slice(0, PRESENCE_LIMIT).map(member => member.id), [members, perspective]);
  const polled = useParticipantsPresence(others, !readOnly && others.length > 0);
  const presenceOf = (account: AccountRef) => polled.get(account.id) ?? presence.get(account.id);
  const onlyOwner = amOwner && (summary?.owners.length ?? 0) <= 1;

  // While the panel shows, the group is asked for again now and then (and when the tab comes back), so members who
  // accepted or left, and another owner's rename, show without reopening it.
  const { refetch } = query;
  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void refetch();
    }, REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [refetch]);
  // What the panel learns (another owner's rename, someone who joined) goes to the thread's header and the list too.
  const queryClient = useQueryClient();
  useEffect(() => {
    if (!group || !conversation) return;
    const joined = group.members.length > 0 && group.members.length !== conversation.participants.length;
    if (joined || summaryDiffers(conversation.group, group)) applyGroupToSummaries(queryClient, address, view, group);
  }, [group, conversation, queryClient, address, view]);

  const memberActions = (member: AccountRef): PersonAction[] => {
    const you = sameAccount(member, perspective);
    const list = basics(member);
    if (canManage) {
      const owner = isListed(member, owners);
      list.push({
        label: owner ? (you ? "Stop being an owner" : "Remove as owner") : "Make owner",
        icon: <Crown size={16} strokeWidth={1.75} />,
        separated: true,
        onSelect: () => {
          actions.setOwner(member.id, !owner).then(
            () => notify.success(owner ? `${you ? "You are" : `${firstName(member)} is`} no longer an owner` : `${firstName(member)} is an owner now`, owner ? undefined : "Owners rename the group, invite and remove members."),
            failure => notifyError(failure, owner ? "Not changed" : `${firstName(member)} is not an owner`),
          );
        },
      });
      if (!you) list.push({ label: "Remove from group", icon: <UserMinus size={16} strokeWidth={1.75} />, destructive: true, onSelect: () => setRemoving(member.id) });
    } else if (!readOnly && me.custodian_of.some(own => sameAccount(own, member))) {
      // A custodian may take its own Silicon out of a group it does not own (the service allows it; so does
      // `dm groups remove`). Not from the read-only "Viewing as" view.
      list.push({ label: "Remove from group", icon: <UserMinus size={16} strokeWidth={1.75} />, destructive: true, separated: true, onSelect: () => setRemoving(member.id) });
    }
    return list;
  };

  const invitedActions = (account: AccountRef): PersonAction[] => [
    { label: "Copy id", icon: <Copy size={16} strokeWidth={1.75} />, onSelect: () => void copyText(account.id, "Id") },
    ...(canManage ? [{ label: "Withdraw invitation", icon: <UserMinus size={16} strokeWidth={1.75} />, destructive: true, separated: true, onSelect: () => setRemoving(account.id) }] : []),
  ];

  const removal = (account: AccountRef, invited: boolean) => ({
    // Short, so the question fits the panel on its own line.
    prompt: invited ? `Uninvite ${firstName(account)}?` : `Remove ${firstName(account)}?`,
    confirmLabel: invited ? "Uninvite" : "Remove",
    pendingLabel: invited ? "Uninviting" : "Removing",
    doneLabel: invited ? "Uninvited" : "Removed",
    run: async () => {
      try {
        await actions.remove([account.id]);
        notify.success(invited ? `${firstName(account)}'s invitation is withdrawn` : `${firstName(account)} is no longer in the group`);
        setRemoving(null);
        requestAnimationFrame(() => document.getElementById("details-members")?.focus());
      } catch (failure) {
        notifyError(failure, invited ? "The invitation was not withdrawn" : `${firstName(account)} was not removed`);
        throw failure;
      }
    },
  });

  const name = summary?.name ?? address;
  const description = summary?.description ?? "";

  return (
    <>
      <section className={styles.section} aria-label="The group">
        {canManage ? (
          <div className={styles.groupHead}>
            {/* The name only: its description takes line breaks (Shift+Enter) as usual. */}
            <div className={styles.nameField} onKeyDownCapture={noLineBreaks}>
              <InlineEdit as="h3" variant="title" multiline label="Group name" value={name} className={styles.groupName}
                validate={next => {
                  // Counted as the service counts: characters, not UTF-16 units (an emoji is one).
                  const length = [...groupNameOf(next)].length;
                  return !length ? "A group needs a name." : length > 120 ? `That is ${length} characters; a name has at most 120.` : null;
                }}
                onSave={async next => {
                  setRenameError(null);
                  try {
                    await actions.update({ name: groupNameOf(next) });
                  } catch (failure) {
                    setRenameError(ApiError.from(failure));
                    throw failure;
                  }
                }} />
            </div>
            <InlineEdit as="p" variant="body" multiline label="Description" value={description} placeholder="Add a description: what the group is for" className={styles.groupDescription}
              validate={next => ([...next].length > 4000 ? `That is ${[...next].length} characters; a description has at most 4000.` : null)}
              onSave={async next => {
                setRenameError(null);
                try {
                  await actions.update({ description: next });
                } catch (failure) {
                  setRenameError(ApiError.from(failure));
                  throw failure;
                }
              }} />
            {/* The field says a save failed and offers Try again; a refusal also gets the service's reason. When the network
                is down the field's own words are enough (one message, not two). */}
            {renameError && renameError.status !== 0 ? <ErrorAlert error={renameError} title="The group was not changed" /> : null}
          </div>
        ) : (
          <div className={styles.groupHead}>
            <h3 className={styles.groupTitle}>{name}</h3>
            {description ? <p className={styles.text}>{description}</p> : null}
          </div>
        )}
      </section>

      <section className={styles.section} aria-labelledby="details-members">
        <div className={styles.sectionHead}>
          <h3 id="details-members" className={styles.heading} tabIndex={-1}>Members <span className={styles.count}>{members.length}</span></h3>
          {canManage && !adding ? (
            <Button ref={addButton} size="sm" variant="ghost" onClick={() => setAdding(true)}><UserPlus size={16} strokeWidth={1.75} aria-hidden="true" />Add members</Button>
          ) : null}
        </div>
        {adding && canManage ? (
          <AddMembers inGroup={inGroup} onAdd={ids => actions.invite(ids)} onDone={() => { setAdding(false); requestAnimationFrame(() => addButton.current?.focus()); }} />
        ) : null}
        {query.error && !members.length ? <ErrorAlert error={ApiError.from(query.error)} title="Members could not be loaded" action={<Button size="sm" variant="secondary" onClick={() => void query.refetch()}>Try again</Button>} /> : null}
        <ul role="list" className={styles.people}>
          {members.map(member => {
            const you = sameAccount(member, perspective);
            const notes = [...(isListed(member, owners) ? ["Owner"] : []), ...(you && !view ? ["You"] : [])];
            return (
              <PersonRow key={member.uuid ?? member.id} account={member} presence={you ? undefined : presenceOf(member)} notes={notes}
                custodian={custodianWords(member, perspective, view ? [] : me.custodian_of)}
                actions={memberActions(member)} removal={removal(member, false)} removing={removing === member.id} onRemovingChange={on => setRemoving(on ? member.id : null)} />
            );
          })}
        </ul>
      </section>

      {group?.pending_invitations.length ? (
        <section className={styles.section} aria-labelledby="details-invited">
          <h3 id="details-invited" className={styles.heading}>Invited <span className={styles.count}>{group.pending_invitations.length}</span></h3>
          <p className={styles.note}>They see the group once they accept.</p>
          <ul role="list" className={styles.people}>
            {group.pending_invitations.map(account => (
              <PersonRow key={account.uuid ?? account.id} account={account} notes={["Invited"]} actions={invitedActions(account)}
                custodian={custodianWords(account, perspective, view ? [] : me.custodian_of)}
                removal={removal(account, true)} removing={removing === account.id} onRemovingChange={on => setRemoving(on ? account.id : null)} />
            ))}
          </ul>
        </section>
      ) : null}

      {readOnly ? null : (
        <section className={styles.section} aria-labelledby="details-leave">
          <h3 id="details-leave" className={styles.heading}>Leave</h3>
          <p className={styles.text}>
            {onlyOwner
              ? members.length > 1
                ? "You are the group's only owner. Make another member an owner, then you can leave."
                : "You are the group's only member and owner, so it stays with you."
              : "You stop receiving its messages. An owner can invite you back."}
          </p>
          <div>
            <HoldToConfirm label="Hold to leave the group" confirmedLabel="Left" tone="danger" disabled={onlyOwner} confirmed={left}
              onConfirm={() => {
                setLeft(true);
                setLeaveError(null);
                actions.leave().then(
                  () => {
                    notify.success(`You left ${name}`);
                    if (details.mode !== "wide") details.setOpen(false);
                    router.push(inboxHref(null));
                  },
                  failure => {
                    setLeft(false);
                    setLeaveError(ApiError.from(failure));
                  },
                );
              }} />
          </div>
          {leaveError ? <ErrorAlert error={leaveError} title="You did not leave the group" /> : null}
        </section>
      )}
    </>
  );
}
