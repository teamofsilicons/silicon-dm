"use client";

/** The chat's dialogs, mounted once; components/chat/dialogs/store.ts says which one is open. */
import { Cheatsheet } from "../keyboard/cheatsheet";
import { InvitationsDialog } from "./invitations-dialog";
import { InviteToChatDialog } from "./invite-to-chat-dialog";
import { NewConversationDialog } from "./new-conversation-dialog";
import { NewGroupDialog } from "./new-group-dialog";
import { SignOutDialog } from "./sign-out-dialog";
import { useOpenDialog } from "./store";

export function ChatDialogs() {
  const open = useOpenDialog();
  return (
    <>
      <NewConversationDialog open={open === "new-conversation"} />
      <NewGroupDialog open={open === "new-group"} />
      <InvitationsDialog open={open === "invitations"} />
      <InviteToChatDialog open={open === "invite-to-chat"} />
      <Cheatsheet open={open === "shortcuts"} />
      <SignOutDialog open={open === "sign-out"} />
    </>
  );
}
