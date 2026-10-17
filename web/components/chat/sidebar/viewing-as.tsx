"use client";

/**
 * "Viewing as" for custodians: switch the list between your own conversations and those of a Silicon you look after
 * (read only: you see what it sees, and nothing is sent or marked read as it).
 */
import { useRouter } from "next/navigation";
import { Check, ChevronsUpDown } from "lucide-react";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuRoot, DropdownMenuTrigger } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { accountName, inboxHref } from "@/lib/chat/address";
import { useChat } from "../shell/chat-context";
import styles from "./sidebar.module.css";

export function ViewingAs() {
  const { me, view, viewed } = useChat();
  const router = useRouter();
  const current = view ? (viewed ? accountName(viewed) : view) : "You";
  return (
    <DropdownMenuRoot>
      <DropdownMenuTrigger asChild>
        <button type="button" className={styles.viewAs} data-sq="surface">
          <span className={styles.viewAsLabel}>Viewing as </span>
          {/* The hint sits inside the value, so the name reads "Viewing as You, change whose…" with no stray stop. */}
          <span className={styles.viewAsValue}>{current}<span className="sr-only">, change whose conversations you see</span></span>
          <ChevronsUpDown size={14} strokeWidth={1.75} aria-hidden="true" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" side="top">
        <DropdownMenuLabel>Whose conversations</DropdownMenuLabel>
        <DropdownMenuItem icon={view ? <span /> : <Check size={16} strokeWidth={2} />} onSelect={() => router.push(inboxHref(null))}>
          You ({me.account.id})
        </DropdownMenuItem>
        {me.custodian_of.map(silicon => (
          <DropdownMenuItem key={silicon.uuid ?? silicon.id} icon={view === silicon.id ? <Check size={16} strokeWidth={2} /> : <span />} onSelect={() => router.push(inboxHref(silicon.id))}>
            {accountName(silicon)}{accountName(silicon) === silicon.id ? "" : ` (${silicon.id})`}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenuRoot>
  );
}
