"use client";

/** "New": a conversation (N) or a group (⇧N), from a small menu beside the wordmark. */
import { MessageSquarePlus, Plus, Users } from "lucide-react";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuTrigger } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { Tooltip } from "@/components/silicon-ui/tooltip/tooltip";
import { useShortcutText } from "@/lib/shortcuts/react";
import { openDialog } from "../dialogs/store";
import styles from "./sidebar.module.css";

export function NewMenu({ disabledReason }: { disabledReason?: string | null }) {
  const keys = useShortcutText();
  if (disabledReason) {
    return (
      <Tooltip content={disabledReason} side="bottom">
        <button type="button" className={styles.newButton} data-sq="surface" aria-disabled="true" aria-label={`New (${disabledReason})`}>
          <Plus size={16} strokeWidth={2} aria-hidden="true" />
          <span>New</span>
        </button>
      </Tooltip>
    );
  }
  return (
    <DropdownMenuRoot>
      <DropdownMenuTrigger asChild>
        <button type="button" className={styles.newButton} data-sq="surface" aria-label="New conversation or group">
          <Plus size={16} strokeWidth={2} aria-hidden="true" />
          <span>New</span>
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem icon={<MessageSquarePlus size={16} strokeWidth={1.75} />} shortcut={keys("conversation.new")} onSelect={() => openDialog("new-conversation")}>New conversation</DropdownMenuItem>
        <DropdownMenuItem icon={<Users size={16} strokeWidth={1.75} />} shortcut={keys("group.new")} onSelect={() => openDialog("new-group")}>New group</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenuRoot>
  );
}
