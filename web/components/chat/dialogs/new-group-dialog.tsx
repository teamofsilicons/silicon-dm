"use client";

/**
 * New group (⇧N, `/c?new=group`): a name, an optional description, and the accounts to invite. You become its first
 * owner; the people you name are invited and join when they accept (your own Silicons join at once). The group's id
 * comes from its name (`g:product-design`).
 */
import { useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { Input } from "@/components/silicon-ui/input/input";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { ShareWithAccounts } from "@/components/foundation/account/share-with-accounts";
import { conversationHref } from "@/lib/chat/address";
import { createGroup } from "@/lib/chat/api";
import { chatKeys } from "@/lib/chat/keys";
import { ApiError } from "@/lib/errors";
import { useChat } from "../shell/chat-context";
import { closeDialog } from "./store";
import styles from "./dialogs.module.css";

export function NewGroupDialog({ open }: { open: boolean }) {
  const { me } = useChat();
  const router = useRouter();
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [ids, setIds] = useState<string[]>([]);
  const [error, setError] = useState<ApiError | null>(null);
  const [busy, setBusy] = useState(false);
  const [key, setKey] = useState(() => crypto.randomUUID());
  const latest = useRef(ids);
  useEffect(() => {
    latest.current = ids;
  }, [ids]);

  const reset = () => {
    setName("");
    setDescription("");
    setIds([]);
    setError(null);
    setBusy(false);
    setKey(crypto.randomUUID());
  };
  const close = () => {
    closeDialog("new-group");
    reset();
  };

  const create = async () => {
    // The id typed last joins the list when its field loses focus.
    await new Promise(resolve => setTimeout(resolve, 0));
    if (!name.trim()) {
      setError(new ApiError({ status: 0, code: "name_required", message: "A group needs a name.", hint: "Give it the name its members will recognise, such as Product design." }));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const group = await createGroup({ name: name.trim(), description: description.trim(), member_ids: latest.current }, key);
      await queryClient.invalidateQueries({ queryKey: chatKeys.conversations() });
      close();
      router.push(conversationHref(group.id));
    } catch (failure) {
      setError(ApiError.from(failure));
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={next => { if (!next) close(); }}>
      <DialogContent title="New group" description="You become its first owner. The accounts you invite join when they accept."
        onOpenAutoFocus={event => { event.preventDefault(); document.getElementById("new-group-name")?.focus(); }}>
        <form className={styles.form} onSubmit={event => { event.preventDefault(); void create(); }}>
          <Input id="new-group-name" label="Name" value={name} onChange={event => setName(event.target.value)} maxLength={120} required autoComplete="off" placeholder="Product design" />
          <Textarea label="Description" description="Optional: what the group is for." value={description} onChange={event => setDescription(event.target.value)} maxLength={4000} rows={2} />
          <ShareWithAccounts label="Invite" description="c: or si: ids; press Enter after each. You can invite more later." ids={ids} onIdsChange={setIds} selfId={me.account.id} max={100} />
          {error ? <ErrorAlert error={error} title="The group could not be created" /> : null}
          <div className={styles.actions}>
            <Button type="button" variant="ghost" onClick={close}>Cancel</Button>
            <Button type="submit" loading={busy}>Create group</Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
