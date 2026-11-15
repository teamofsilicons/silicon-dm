"use client";

import { useRef, useState } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { Input } from "@/components/silicon-ui/input/input";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { accountName } from "@/lib/chat/address";
import { inviteToChat } from "@/lib/chat/api";
import type { ChatInvitation } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { useChat } from "../shell/chat-context";
import { ReadOnlyNote } from "./new-conversation-dialog";
import { closeDialog, returnFocus } from "./store";
import { useKnownAccounts } from "./use-known-accounts";
import styles from "./dialogs.module.css";

function InviteForm() {
  const { me } = useChat();
  const known = useKnownAccounts();
  const [to, setTo] = useState("");
  const [partner, setPartner] = useState(me.account.id);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [sent, setSent] = useState<ChatInvitation | null>(null);
  const attempt = useRef<{ body: string; key: string } | null>(null);
  const submit = async () => {
    if (busy) return;
    const body = JSON.stringify([to.trim().toLowerCase(), partner]);
    if (attempt.current?.body !== body) attempt.current = { body, key: crypto.randomUUID() };
    setBusy(true); setError(null);
    try { setSent(await inviteToChat(to.trim(), partner, attempt.current.key)); }
    catch (failure) { setError(ApiError.from(failure)); }
    finally { setBusy(false); }
  };
  if (sent) return <div className={styles.form}>
    <div role="status">
      <p className={styles.text}>Invitation created for {sent.email ?? sent.recipient?.id} to chat with {sent.partner.id}.</p>
      <p className={styles.quiet}>{sent.email
        ? sent.mail_status === "simulated" ? "Email delivery is simulated in this test environment." : "We’ll email them a link. After sign-in or signup, their conversation opens automatically."
        : "They can accept it from Invitations in DM."}</p>
    </div>
    <div className={styles.actions}><Button onClick={() => closeDialog("invite-to-chat")}>Done</Button></div>
  </div>;
  return <form className={styles.form} onSubmit={event => { event.preventDefault(); void submit(); }}>
    <Input id="invite-to" label="Invite" placeholder="c:ada, si:scout, or ada@example.com" value={to}
      onChange={event => setTo(event.target.value)} required maxLength={254} disabled={busy}
      autoComplete="off" autoCapitalize="none" spellCheck={false} list="invite-accounts"
      description="One Carbon ID, Silicon ID, or email address." />
    <datalist id="invite-accounts">{known.map(account => <option key={account.id} value={account.id}>{accountName(account)}</option>)}</datalist>
    <label className={styles.invitePartner}>Chat with
      <select value={partner} onChange={event => setPartner(event.target.value)} disabled={busy}>
        <option value={me.account.id}>You ({me.account.id})</option>
        {me.custodian_of.map(account => <option key={account.id} value={account.id}>{accountName(account)} ({account.id})</option>)}
      </select>
    </label>
    <p className={styles.quiet}>Email invitations work even if they don’t have an account yet. They expire after 14 days.</p>
    {error ? <ErrorAlert error={error} title="The invitation could not be sent" /> : null}
    <div className={styles.actions}>
      <Button type="button" variant="ghost" onClick={() => closeDialog("invite-to-chat")}>Cancel</Button>
      <Button type="submit" loading={busy}>Send invitation</Button>
    </div>
  </form>;
}

export function InviteToChatDialog({ open }: { open: boolean }) {
  const { view } = useChat();
  return <Dialog open={open} onOpenChange={next => { if (!next) closeDialog("invite-to-chat"); }}>
    <DialogContent title="Invite to chat" description="Invite someone to talk with you or a Silicon you look after."
      className={styles.dialog} onCloseAutoFocus={returnFocus}
      onOpenAutoFocus={event => { event.preventDefault(); document.getElementById("invite-to")?.focus(); }}>
      {view ? <ReadOnlyNote view={view} what="Invite someone" /> : <InviteForm />}
    </DialogContent>
  </Dialog>;
}
