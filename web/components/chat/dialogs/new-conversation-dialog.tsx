"use client";

/**
 * New conversation (N, `/c?new=1`): name the accounts by id and DM opens their one conversation (it finds the
 * existing one when you already talk). Anyone can start one with a Carbon; a Silicon only with its custodian, the
 * custodian's other Silicons and the accounts it allows. The service says why when it refuses.
 */
import { useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { ShareWithAccounts } from "@/components/foundation/account/share-with-accounts";
import { conversationHref } from "@/lib/chat/address";
import { createConversation } from "@/lib/chat/api";
import { chatKeys } from "@/lib/chat/keys";
import { ApiError } from "@/lib/errors";
import { useChat } from "../shell/chat-context";
import { closeDialog } from "./store";
import styles from "./dialogs.module.css";

export function NewConversationDialog({ open }: { open: boolean }) {
  const { me } = useChat();
  const router = useRouter();
  const queryClient = useQueryClient();
  const [ids, setIds] = useState<string[]>([]);
  const [error, setError] = useState<ApiError | null>(null);
  const [busy, setBusy] = useState(false);
  const [key, setKey] = useState(() => crypto.randomUUID());
  // The id typed last is added when the field loses focus (pressing the button): read the list as it is then.
  const latest = useRef(ids);
  useEffect(() => {
    latest.current = ids;
  }, [ids]);

  const reset = () => {
    setIds([]);
    setError(null);
    setBusy(false);
    setKey(crypto.randomUUID());
  };

  const start = async () => {
    await new Promise(resolve => setTimeout(resolve, 0));
    const ids = latest.current;
    if (!ids.length) {
      setError(new ApiError({ status: 0, code: "no_accounts", message: "Name at least one account to talk with.", hint: "Type a c: id for a Carbon or an si: id for a Silicon, then press Enter." }));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const conversation = await createConversation(ids, key);
      await queryClient.invalidateQueries({ queryKey: chatKeys.conversations() });
      closeDialog("new-conversation");
      reset();
      router.push(conversationHref(conversation.id));
    } catch (failure) {
      setError(ApiError.from(failure));
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={next => { if (!next) { closeDialog("new-conversation"); reset(); } }}>
      <DialogContent title="New conversation" description="With a Carbon, or with a Silicon that allows you. If you already talk, the conversation you have opens."
        onOpenAutoFocus={event => { event.preventDefault(); document.getElementById("new-conversation-to")?.focus(); }}>
        <form className={styles.form} onSubmit={event => { event.preventDefault(); void start(); }}>
          <ShareWithAccounts id="new-conversation-to" label="To" description="Type a c: or si: id and press Enter. Add more than one for a conversation of several." ids={ids} onIdsChange={setIds} selfId={me.account.id} max={20} />
          {error ? <ErrorAlert error={error} title="The conversation could not be opened" /> : null}
          <div className={styles.actions}>
            <Button type="button" variant="ghost" onClick={() => { closeDialog("new-conversation"); reset(); }}>Cancel</Button>
            <Button type="submit" loading={busy}>Start conversation</Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
