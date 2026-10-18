"use client";

/**
 * Faces in the chat: an account's profile photo (initials while it loads or when there is none) as a 30 % squircle,
 * with a presence dot when it is known, and a group's mark from its name. Built on Silicon UI's Avatar; the sizes the
 * chat needs (40 px rows, 32 px headers, 28 px bubbles) are set here.
 */
import { Avatar } from "@/components/silicon-ui/avatar/avatar";
import { accountName } from "@/lib/chat/address";
import type { AccountRef, Presence } from "@/lib/chat/types";
import styles from "./common.module.css";

export type FaceSize = "row" | "header" | "bubble" | "small";

const BASE: Record<FaceSize, "sm" | "md" | "lg"> = { row: "md", header: "md", bubble: "sm", small: "sm" };

const nameForInitials = (account: Pick<AccountRef, "id" | "display_name" | "deleted">) => {
  const name = accountName(account);
  return name === account.id ? account.id.replace(/^(c|si):/, "").replace(/[-_]+/g, " ") : name;
};

interface FaceProps {
  size?: FaceSize;
  className?: string;
  /** The name is already said next to the face (a row, a header): hidden from assistive technology. */
  decorative?: boolean;
}

export function AccountAvatar({ account, size = "row", presence, className, decorative }: FaceProps & { account: AccountRef; presence?: Presence | null }) {
  const status = presence ? (presence.availability === "online" ? "online" : "offline") : undefined;
  return (
    <Avatar
      name={nameForInitials(account)}
      src={account.pfp_url}
      size={BASE[size]}
      status={status}
      className={[styles.face, className].filter(Boolean).join(" ")}
      data-size={size}
      data-kind={account.type}
      aria-hidden={decorative ? true : undefined}
    />
  );
}

/** A group's face: the initials of its name on the beige of the current item. */
export function GroupAvatar({ name, size = "row", className, decorative }: FaceProps & { name: string }) {
  return <Avatar name={name} size={BASE[size]} className={[styles.face, styles.groupFace, className].filter(Boolean).join(" ")} data-size={size} aria-hidden={decorative ? true : undefined} />;
}

/** The face of a conversation: the group's, or the one other account's. */
export function ConversationAvatar({ group, others, presence, ...face }: FaceProps & { group?: { name: string } | null; others: AccountRef[]; presence?: Presence | null }) {
  if (group || others.length !== 1) return <GroupAvatar name={group?.name ?? others.map(accountName).join(" ")} {...face} />;
  return <AccountAvatar account={others[0]!} presence={presence} {...face} />;
}
