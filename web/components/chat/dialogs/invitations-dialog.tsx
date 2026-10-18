"use client";

/**
 * Invitations to groups (G then I, the palette, the sidebar's invitations line): what the group is, who invited you and
 * when; Accept joins it (with its whole history) and opens it, Decline lets the invitation go. In a custodian's view
 * these are the Silicon's invitations: a custodian may decline for it, and only the Silicon itself accepts.
 */
import { useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { accountName } from "@/lib/chat/address";
import { acceptInvitation, declineInvitation } from "@/lib/chat/api";
import { fullTime } from "@/lib/chat/format";
import { useInvitations } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { announceConversationsChanged } from "@/lib/chat/news";
import type { Invitation } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { formatRelative } from "@/lib/format";
import { notify } from "@/lib/notify";
import { GroupAvatar } from "../common/avatars";
import { useChat } from "../shell/chat-context";
import { useOpenConversation } from "./open-conversation";
import { closeDialog, forgetOpener, returnFocus } from "./store";
import styles from "./dialogs.module.css";

function InvitationRow({ invitation }: { invitation: Invitation }) {
  const { me, view, viewed, readOnly } = useChat();
  const viewedName = viewed ? accountName(viewed) : view;
  const queryClient = useQueryClient();
  const open = useOpenConversation();
  const [busy, setBusy] = useState<"accept" | "decline" | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const row = useRef<HTMLLIElement>(null);
  const key = chatKeys.invitations(view);

  const accept = async () => {
    setBusy("accept");
    setError(null);
    try {
      const group = await acceptInvitation(invitation.conversation_id);
      queryClient.setQueryData<{ items: Invitation[] }>(key, data => data && { items: data.items.filter(item => item.conversation_id !== invitation.conversation_id) });
      forgetOpener();
      closeDialog("invitations");
      notify.success(`You joined ${invitation.name}`);
      open(group);
    } catch (failure) {
      setError(ApiError.from(failure));
      setBusy(null);
    }
  };

  const decline = async () => {
    setBusy("decline");
    setError(null);
    try {
      await declineInvitation(invitation.conversation_id, view);
      // The row goes; the keyboard moves on to the next invitation (or the one before it, or the dialog's own close
      // button when it was the last), instead of falling to the dialog itself.
      const node = row.current;
      const neighbour = (node?.nextElementSibling ?? node?.previousElementSibling) as HTMLElement | null;
      const layer = node?.closest<HTMLElement>('[role="dialog"]') ?? null;
      queryClient.setQueryData<{ items: Invitation[] }>(key, data => data && { items: data.items.filter(item => item.conversation_id !== invitation.conversation_id) });
      if (!view) announceConversationsChanged(me.account.uuid ?? me.account.id);
      requestAnimationFrame(() => (neighbour?.isConnected ? neighbour.querySelector<HTMLElement>("button") : layer?.querySelector<HTMLElement>("button"))?.focus());
      notify.info(view ? `${viewedName} declined ${invitation.name}` : `You declined ${invitation.name}`);
    } catch (failure) {
      setError(ApiError.from(failure));
      setBusy(null);
    }
  };

  const by = invitation.invited_by ?? invitation.owners[0] ?? null;
  const who = viewedName ?? "you";
  // Who and when on one line (the separator glued to the words before it, so a line that wraps never starts with one,
  // and the time whole), the group's address under them.
  const invited = by ? `${accountName(by)} invited ${who}` : `Invited ${who}`;
  return (
    <li ref={row} className={styles.invitation} data-invitation={invitation.conversation_id}>
      <GroupAvatar name={invitation.name} size="header" decorative />
      <div className={styles.invitationText}>
        <p className={styles.invitationName}>{invitation.name}</p>
        {invitation.description ? <p className={styles.invitationDescription}>{invitation.description}</p> : null}
        <p className={styles.invitationMeta}>
          {invitation.invited_at ? (
            <>
              {`${invited}\u00a0·`}{" "}
              <time className={styles.nowrap} dateTime={invitation.invited_at} title={fullTime(invitation.invited_at)}>{formatRelative(invitation.invited_at)}</time>
            </>
          ) : invited}
        </p>
        <p className={styles.invitationMeta}><span className={styles.mono}>{invitation.conversation_id}</span></p>
      </div>
      <div className={styles.invitationActions}>
        <Button size="sm" variant="ghost" loading={busy === "decline"} disabled={!!busy} onClick={() => void decline()}
          aria-label={readOnly ? `Decline ${invitation.name} for ${viewedName}` : `Decline ${invitation.name}`}>Decline</Button>
        {/* Joining is what an invitation is for: Accept is the row's one primary action. */}
        {readOnly ? null : <Button size="sm" loading={busy === "accept"} disabled={!!busy} onClick={() => void accept()}>Accept</Button>}
      </div>
      {error ? <div className={styles.invitationError}><ErrorAlert error={error} /></div> : null}
    </li>
  );
}

export function InvitationsDialog({ open }: { open: boolean }) {
  const { view, viewed } = useChat();
  const invitations = useInvitations(view);
  const name = viewed ? accountName(viewed) : view;
  const items = invitations.data?.items ?? [];
  return (
    <Dialog open={open} onOpenChange={next => { if (!next) closeDialog("invitations"); }}>
      <DialogContent title="Invitations"
        description={view ? `Groups ${name} is invited to. You may decline for it; only ${name} can accept.` : "Groups you are invited to. Joining shows you their whole history."}
        className={styles.dialog} onCloseAutoFocus={returnFocus}>
        {/* A failed refresh keeps what was loaded (it is asked for again soon); only a list never loaded says why. */}
        {invitations.error && !invitations.data ? <ErrorAlert error={ApiError.from(invitations.error)} title="Invitations could not be loaded" action={<Button size="sm" variant="secondary" onClick={() => void invitations.refetch()}>Try again</Button>} /> : null}
        {items.length ? (
          <ul role="list" className={styles.invitations} aria-label={`${items.length} ${items.length === 1 ? "invitation" : "invitations"}`}>
            {items.map(invitation => <InvitationRow key={invitation.conversation_id} invitation={invitation} />)}
          </ul>
        ) : invitations.isPending ? (
          <p className={styles.quiet} role="status">Looking for invitations…</p>
        ) : invitations.error && !invitations.data ? null : (
          <p className={styles.quiet} role="status">No invitations are waiting. When someone invites {name ?? "you"} to a group, it shows here.</p>
        )}
      </DialogContent>
    </Dialog>
  );
}
