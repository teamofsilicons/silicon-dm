"use client";

import { useCallback, useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { ButtonLink } from "@/components/foundation/button-link";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { accountName } from "@/lib/chat/address";
import { answerChatInvitation, getConversation } from "@/lib/chat/api";
import { chatKeys } from "@/lib/chat/keys";
import type { ChatInvitation } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { formatRelative } from "@/lib/format";
import { AccountAvatar } from "../common/avatars";
import { closeDialog, forgetOpener } from "./store";
import { useOpenConversation } from "./open-conversation";
import styles from "./dialogs.module.css";

function useAnswer() {
  const client = useQueryClient();
  const open = useOpenConversation();
  return useCallback(async (id: string, answer: "accept" | "decline", stillHere: () => boolean = () => true) => {
    const result = await answerChatInvitation(id, answer);
    void client.invalidateQueries({ queryKey: chatKeys.chatInvitations });
    if (result.conversation_id && answer === "accept") {
      const conversation = await getConversation(result.conversation_id);
      if (stillHere()) { forgetOpener(); closeDialog("invitations"); open(conversation); }
    }
  }, [client, open]);
}

export function ChatInvitationRow({ invitation }: { invitation: ChatInvitation }) {
  const answer = useAnswer();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const run = async (decision: "accept" | "decline") => {
    if (busy) return;
    setBusy(true); setError(null);
    try { await answer(invitation.id, decision); }
    catch (failure) { setError(ApiError.from(failure)); }
    finally { setBusy(false); }
  };
  return <li className={styles.invitation} data-chat-invitation={invitation.id}>
    <AccountAvatar account={invitation.partner} size="header" decorative />
    <div className={styles.invitationText}>
      <p className={styles.invitationName}>Chat with {accountName(invitation.partner)}</p>
      <p className={styles.invitationMeta}>{accountName(invitation.inviter)} ({invitation.inviter.id}) invited you to talk with {invitation.partner.id}.</p>
      <p className={styles.invitationMeta}>Expires {formatRelative(invitation.expires_at)}</p>
    </div>
    <div className={styles.invitationActions}>
      <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run("decline")}>Decline</Button>
      <Button size="sm" disabled={busy} onClick={() => void run("accept")}>Open chat</Button>
    </div>
    {error ? <div className={styles.invitationError}><ErrorAlert error={error} /></div> : null}
  </li>;
}

/** The signed-in destination of the email's Open chat link, including after signup. */
export function OpenChatInvitation({ id }: { id: string }) {
  const answer = useAnswer();
  const [retry, setRetry] = useState(0);
  const [error, setError] = useState<ApiError | null>(null);
  useEffect(() => {
    let here = true;
    void answer(id, "accept", () => here).catch(failure => { if (here) setError(ApiError.from(failure)); });
    return () => { here = false; };
  }, [id, answer, retry]);
  const returnTo = `/invite/${encodeURIComponent(id)}`;
  return <section className={styles.inviteLanding}>
    <h1>{error ? "Chat invitation" : "Opening your chat…"}</h1>
    {error ? <>
      <ErrorAlert error={error} />
      <div className={styles.actions}>
        <Button variant="secondary" onClick={() => { setError(null); setRetry(value => value + 1); }}>Try again</Button>
        {error.code === "invitation_recipient_required" ? <ButtonLink href={`/auth/sign-in?prompt=select_account&return_to=${encodeURIComponent(returnTo)}`}>Sign in to the invited account</ButtonLink> : <ButtonLink href="/c">Your conversations</ButtonLink>}
      </div>
    </> : <p className={styles.quiet} role="status">Checking your invitation and preparing the conversation.</p>}
  </section>;
}
