"use client";

/**
 * New conversation (N, `/c?new=1`): choose the accounts (people you talk to, or any id) and DM opens their one
 * conversation; when you already talk, the conversation you have opens. Anyone signed in may start one with any Carbon
 * or Silicon. A refusal (an id that names nobody, or the hour's new conversations used up) is shown in the service's
 * words, and the accounts it names are marked.
 *
 * In a custodian's "Viewing as" the website is read only: the dialog says so and offers the way back.
 */
import { useRef, useState } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { ButtonLink } from "@/components/foundation/button-link";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { accountName, inboxHref } from "@/lib/chat/address";
import type { AccountRef } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { useChat } from "../shell/chat-context";
import { AccountPicker, type AccountPickerHandle } from "./account-picker";
import { idsNamedIn } from "./contacts";
import { useStartConversation } from "./open-conversation";
import { useShowWhenSet } from "./show-when-set";
import { closeDialog, forgetOpener, returnFocus } from "./store";
import { useKnownAccounts } from "./use-known-accounts";
import styles from "./dialogs.module.css";

const FIELD = "new-conversation-to";

function NewConversationForm() {
  const { me } = useChat();
  const known = useKnownAccounts();
  const start = useStartConversation();
  const picker = useRef<AccountPickerHandle>(null);
  const [accounts, setAccounts] = useState<AccountRef[]>([]);
  const [error, setError] = useState<ApiError | null>(null);
  const [busy, setBusy] = useState(false);
  const errorBox = useShowWhenSet<HTMLDivElement>(error);
  // One Idempotency-Key per set of accounts: a retry of the same set is the same request; a changed set is a new one.
  const attempt = useRef<{ ids: string; key: string } | null>(null);

  const submit = async () => {
    // One request at a time: Enter in the field while one is on its way does nothing.
    if (busy) return;
    const { accounts: chosen, problem } = picker.current?.commit() ?? { accounts, problem: null };
    if (problem) return;
    if (!chosen.length) {
      setError(new ApiError({ status: 0, code: "no_accounts", message: "Choose who the conversation is with first.", hint: "Type a name or an id (c: for a Carbon, si: for a Silicon) and press Enter, or pick someone from the list." }));
      picker.current?.focus();
      return;
    }
    const ids = chosen.map(account => account.id);
    const signature = [...ids].sort().join(" ");
    if (attempt.current?.ids !== signature) attempt.current = { ids: signature, key: crypto.randomUUID() };
    setBusy(true);
    setError(null);
    try {
      forgetOpener();
      await start(ids, attempt.current.key);
      closeDialog("new-conversation");
    } catch (failure) {
      setError(ApiError.from(failure));
      setBusy(false);
    }
  };

  return (
    <form className={styles.form} onSubmit={event => { event.preventDefault(); void submit(); }}>
      <AccountPicker ref={picker} id={FIELD} label="To" value={accounts} onChange={next => { setAccounts(next); setError(null); }}
        suggestions={known} exclude={[me.account]} excludedReason={account => `${account.id} is you. Choose who you want to talk with.`}
        flagged={error ? idsNamedIn(`${error.message} ${error.hint ?? ""}`, accounts) : []}
        description="Choose more than one for a conversation of several." />
      {error ? <div ref={errorBox}><ErrorAlert error={error} title="The conversation could not be opened" /></div> : null}
      <div className={styles.actions}>
        <Button type="button" variant="ghost" onClick={() => closeDialog("new-conversation")}>Cancel</Button>
        <Button type="submit" loading={busy}>Start conversation</Button>
      </div>
    </form>
  );
}

/**
 * What a custodian sees in a Silicon's read-only view: starting a conversation is something you do as yourself. One
 * action, the way back (the dialog's ✕ and Esc close it); a Cancel beside it would wrap onto a line of its own on a phone.
 */
export function ReadOnlyNote({ view, what }: { view: string; what: string }) {
  const { viewed } = useChat();
  const name = viewed ? accountName(viewed) : view;
  return (
    <div className={styles.form}>
      <p className={styles.text}>You are viewing {name}&apos;s conversations as its custodian, read only. {what} as yourself from your own conversations.</p>
      <div className={styles.actions}>
        <ButtonLink href={inboxHref(null)} onClick={() => closeDialog()}>Back to your conversations</ButtonLink>
      </div>
    </div>
  );
}

export function NewConversationDialog({ open }: { open: boolean }) {
  const { view } = useChat();
  return (
    <Dialog open={open} onOpenChange={next => { if (!next) closeDialog("new-conversation"); }}>
      <DialogContent title="New conversation" description="With a Carbon or a Silicon. If you already talk, that conversation opens."
        className={styles.dialog}
        onOpenAutoFocus={event => { event.preventDefault(); document.getElementById(FIELD)?.focus(); }}
        onCloseAutoFocus={returnFocus}>
        {view ? <ReadOnlyNote view={view} what="Start a conversation" /> : <NewConversationForm />}
      </DialogContent>
    </Dialog>
  );
}
