"use client";

/**
 * Invitations to groups (G I): accept to join (and see the whole history), or decline. In a custodian's view, these
 * are the Silicon's invitations: a custodian may decline for it, and only the Silicon itself accepts.
 */
import { useRouter } from "next/navigation";
import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { accountName, conversationHref } from "@/lib/chat/address";
import { acceptInvitation, declineInvitation } from "@/lib/chat/api";
import { useInvitations } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import type { Invitation } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { notify } from "@/lib/notify";
import { useChat } from "../shell/chat-context";
import { closeDialog } from "./store";
import styles from "./dialogs.module.css";

function InvitationRow({ invitation }: { invitation: Invitation }) {
  const { view, readOnly } = useChat();
  const router = useRouter();
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState<"accept" | "decline" | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const settle = async () => {
    await Promise.all([queryClient.invalidateQueries({ queryKey: chatKeys.invitations(view) }), queryClient.invalidateQueries({ queryKey: chatKeys.conversations(view) })]);
  };
  const act = async (what: "accept" | "decline") => {
    setBusy(what);
    setError(null);
    try {
      if (what === "accept") {
        const group = await acceptInvitation(invitation.conversation_id);
        await settle();
        closeDialog("invitations");
        notify.success("You joined", invitation.name);
        router.push(conversationHref(group.id ?? invitation.conversation_id));
      } else {
        await declineInvitation(invitation.conversation_id, view);
        await settle();
      }
    } catch (failure) {
      setError(ApiError.from(failure));
    } finally {
      setBusy(null);
    }
  };
  const from = invitation.invited_by ? accountName(invitation.invited_by) : invitation.owners[0] ? accountName(invitation.owners[0]) : "an owner";
  return (
    <li className={styles.invitation}>
      <div className={styles.invitationText}>
        <p className={styles.invitationName}>{invitation.name}</p>
        {invitation.description ? <p className={styles.invitationDescription}>{invitation.description}</p> : null}
        <p className={styles.invitationMeta}>{invitation.conversation_id} · invited by {from}</p>
      </div>
      <div className={styles.invitationActions}>
        <Button size="sm" variant="secondary" loading={busy === "decline"} disabled={!!busy} onClick={() => void act("decline")}>Decline</Button>
        {readOnly ? null : <Button size="sm" loading={busy === "accept"} disabled={!!busy} onClick={() => void act("accept")}>Accept</Button>}
      </div>
      {error ? <div className={styles.invitationError}><ErrorAlert error={error} /></div> : null}
    </li>
  );
}

export function InvitationsDialog({ open }: { open: boolean }) {
  const { view } = useChat();
  const invitations = useInvitations(view);
  const items = invitations.data?.items ?? [];
  return (
    <Dialog open={open} onOpenChange={next => { if (!next) closeDialog("invitations"); }}>
      <DialogContent title="Invitations" description={view ? `Groups ${view} is invited to. You may decline for it; only ${view} can accept.` : "Groups you are invited to. Joining shows you their whole history."}>
        {invitations.error ? <ErrorAlert error={ApiError.from(invitations.error)} title="Invitations could not be loaded" /> : null}
        {items.length ? (
          <ul role="list" className={styles.invitations}>
            {items.map(invitation => <InvitationRow key={invitation.conversation_id} invitation={invitation} />)}
          </ul>
        ) : invitations.isPending ? (
          <p className={styles.quiet}>Looking for invitations…</p>
        ) : (
          <p className={styles.quiet}>No invitations are waiting.</p>
        )}
      </DialogContent>
    </Dialog>
  );
}
