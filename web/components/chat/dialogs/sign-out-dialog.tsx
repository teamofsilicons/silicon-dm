"use client";

/**
 * Signing out of DM from the chat (the account menu, ⌘K → Sign out). It removes the Carbon's own words from this
 * browser (lib/chat/local-data.ts): drafts, files waiting in a composer, and messages not sent yet. A message is never
 * thrown away without the Carbon's say (docs/learn/drafts-and-outbox.md), so with messages still waiting or failed the
 * dialog says how many first, and staying signed in is where the keyboard starts.
 */
import { useSyncExternalStore } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { useSignOut } from "@/lib/client/session";
import { unsentCount, useOutbox } from "@/lib/chat/outbox-store";
import { notifyError } from "@/lib/notify";
import { closeDialog, openDialog, returnFocus } from "./store";
import styles from "./dialogs.module.css";

const SAFE = "sign-out-stay";

/** Signs out at once, or first asks when messages have not gone yet. Resolves when it has signed out or asked. */
export async function askToSignOut(signOut: () => Promise<void>): Promise<void> {
  if (unsentCount() > 0) {
    openDialog("sign-out");
    return;
  }
  try {
    await signOut();
  } catch (failure) {
    notifyError(failure, "Could not sign out");
  }
}

const subscribeNothing = () => () => undefined;

export function SignOutDialog({ open }: { open: boolean }) {
  const outbox = useOutbox();
  const hydrated = useSyncExternalStore(subscribeNothing, () => true, () => false);
  const { signOut, pending } = useSignOut();
  const count = hydrated ? outbox.length : 0;
  const them = count === 1 ? "it" : "them";
  return (
    <Dialog open={open} onOpenChange={next => { if (!next) closeDialog("sign-out"); }}>
      <DialogContent title="Sign out with messages not sent?"
        description={`${count === 1 ? "1 message has" : `${count} messages have`} not been sent yet. Signing out removes ${them}, and your drafts, from this browser.`}
        className={styles.dialog}
        onOpenAutoFocus={event => { event.preventDefault(); document.getElementById(SAFE)?.focus(); }}
        onCloseAutoFocus={returnFocus}>
        <div className={styles.form}>
          <p className={styles.text}>Stay signed in to keep {them}: a waiting message goes as soon as DM can be reached, and a failed one has Retry and Discard in its conversation.</p>
          <div className={styles.actions}>
            <Button id={SAFE} type="button" variant="secondary" onClick={() => closeDialog("sign-out")}>Stay signed in</Button>
            <Button type="button" variant="danger" loading={pending} onClick={() => void signOut().catch(failure => notifyError(failure, "Could not sign out"))}>Sign out anyway</Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
