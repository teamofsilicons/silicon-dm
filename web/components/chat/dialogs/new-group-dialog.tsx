"use client";

/**
 * New group (⇧N, `/c?new=group`): a name, an optional description, and the accounts to invite. You become its first
 * owner; the people you name are invited and join when they accept (your own Silicons join at once). The group's
 * address comes from its name (`g:product-design`, `-2` when that is taken) and never changes, even on a rename.
 */
import { useRef, useState } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { Input } from "@/components/silicon-ui/input/input";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { createGroup } from "@/lib/chat/api";
import type { AccountRef } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { useChat } from "../shell/chat-context";
import { AccountPicker, type AccountPickerHandle } from "./account-picker";
import { idsNamedIn } from "./contacts";
import { ReadOnlyNote } from "./new-conversation-dialog";
import { useOpenConversation } from "./open-conversation";
import { useShowWhenSet } from "./show-when-set";
import { closeDialog, forgetOpener, returnFocus } from "./store";
import { useKnownAccounts } from "./use-known-accounts";
import styles from "./dialogs.module.css";

const NAME = "new-group-name";

/** The address DM gives a group of this name (the service adds -2, -3… when it is taken). */
export function groupSlug(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}

function NewGroupForm() {
  const { me } = useChat();
  const known = useKnownAccounts();
  const open = useOpenConversation();
  const picker = useRef<AccountPickerHandle>(null);
  const [name, setName] = useState("");
  const [nameProblem, setNameProblem] = useState<string | null>(null);
  const [description, setDescription] = useState("");
  const [accounts, setAccounts] = useState<AccountRef[]>([]);
  const [error, setError] = useState<ApiError | null>(null);
  const [busy, setBusy] = useState(false);
  const errorBox = useShowWhenSet<HTMLDivElement>(error);
  const attempt = useRef<{ body: string; key: string } | null>(null);
  const slug = groupSlug(name);

  const submit = async () => {
    if (busy) return;
    const { accounts: invited, problem } = picker.current?.commit() ?? { accounts, problem: null };
    if (!name.trim()) {
      setNameProblem("Give the group a name its members will recognise, such as Product design.");
      document.getElementById(NAME)?.focus();
      return;
    }
    if (!slug) {
      setNameProblem("A group's address is made from its name, so the name needs a letter or a digit (a to z, 0 to 9).");
      document.getElementById(NAME)?.focus();
      return;
    }
    if (problem) return;
    const body = { name: name.trim(), description: description.trim(), member_ids: invited.map(account => account.id) };
    const signature = JSON.stringify(body);
    if (attempt.current?.body !== signature) attempt.current = { body: signature, key: crypto.randomUUID() };
    setBusy(true);
    setError(null);
    try {
      const group = await createGroup(body, attempt.current.key);
      forgetOpener();
      closeDialog("new-group");
      open(group);
    } catch (failure) {
      setError(ApiError.from(failure));
      setBusy(false);
    }
  };

  return (
    <form className={styles.form} onSubmit={event => { event.preventDefault(); void submit(); }}>
      <Input id={NAME} label="Name" value={name} maxLength={120} required autoComplete="off" placeholder="Product design"
        error={nameProblem ?? undefined}
        description={slug ? `Its address will be g:${slug} (g:${slug}-2 if that is taken), and stays the same after a rename.` : "Its address is made from the name and never changes."}
        onChange={event => { setName(event.target.value); setNameProblem(null); }} />
      <Textarea label="Description" description="Optional: what the group is for." value={description} onChange={event => setDescription(event.target.value)} maxLength={4000} rows={2} />
      <AccountPicker ref={picker} label="Invite" value={accounts} onChange={next => { setAccounts(next); setError(null); }}
        suggestions={known} exclude={[me.account]} excludedReason={account => `${account.id} is you: you are the group's first owner.`} listSize={4}
        flagged={error ? idsNamedIn(`${error.message} ${error.hint ?? ""}`, accounts) : []}
        description="They join when they accept; your own Silicons join at once. You can invite more later." />
      {error ? <div ref={errorBox}><ErrorAlert error={error} title="The group could not be created" /></div> : null}
      <div className={styles.actions}>
        <Button type="button" variant="ghost" onClick={() => closeDialog("new-group")}>Cancel</Button>
        <Button type="submit" loading={busy}>Create group</Button>
      </div>
    </form>
  );
}

export function NewGroupDialog({ open }: { open: boolean }) {
  const { view } = useChat();
  return (
    <Dialog open={open} onOpenChange={next => { if (!next) closeDialog("new-group"); }}>
      <DialogContent title="New group" description="You become its first owner. The accounts you invite join when they accept."
        className={styles.dialog}
        onOpenAutoFocus={event => { event.preventDefault(); document.getElementById(NAME)?.focus(); }}
        onCloseAutoFocus={returnFocus}>
        {view ? <ReadOnlyNote view={view} what="Create a group" /> : <NewGroupForm />}
      </DialogContent>
    </Dialog>
  );
}
