"use client";

/**
 * The message menu: right click a message, press its ⋯, or the menu key / Shift+F10 on a selected message. One menu
 * for the whole thread (Silicon UI's dropdown), opened at the pointer or under the message, with every action and its
 * keys. Focus goes back to the message when it closes.
 */
import { Braces, Copy, CornerUpLeft, Headphones, Link2, Pencil, Trash2 } from "lucide-react";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import type { Message } from "@/lib/chat/types";
import { useShortcutText } from "@/lib/shortcuts/react";
import { canListen, toggleListen, useVoiceAvailability } from "@/lib/voice";
import { useThreadActions } from "./thread-actions";
import styles from "./thread.module.css";

export interface MenuState {
  message: Message;
  x: number;
  y: number;
}

export function MessageMenu({ state, onClose }: { state: MenuState | null; onClose: () => void }) {
  const actions = useThreadActions();
  const keys = useShortcutText();
  const voice = useVoiceAvailability();
  const message = state?.message ?? null;
  const mine = message ? actions.isMine(message) : false;
  const deleted = !!message?.deleted_at;
  const returnFocus = () => {
    const code = message?.["message-id"];
    if (code) document.querySelector<HTMLElement>(`[data-message-code="${CSS.escape(code)}"]`)?.focus({ preventScroll: true });
  };
  return (
    <DropdownMenuRoot open={!!state} onOpenChange={open => { if (!open) onClose(); }} modal={false}>
      <DropdownMenuTrigger asChild>
        <span className={styles.menuAnchor} style={{ left: state?.x ?? 0, top: state?.y ?? 0 }} aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="bottom" onCloseAutoFocus={event => { event.preventDefault(); returnFocus(); }} aria-label="Message actions">
        {message && !deleted ? (
          <>
            {actions.readOnly ? null : <DropdownMenuItem icon={<CornerUpLeft size={16} strokeWidth={1.75} />} shortcut={keys("message.reply")} onSelect={() => actions.reply(message)}>Reply</DropdownMenuItem>}
            {mine && !actions.readOnly ? <DropdownMenuItem icon={<Pencil size={16} strokeWidth={1.75} />} shortcut={keys("message.edit")} onSelect={() => actions.edit(message)}>Edit</DropdownMenuItem> : null}
            <DropdownMenuItem icon={<Copy size={16} strokeWidth={1.75} />} shortcut={keys("message.copy")} onSelect={() => actions.copyText(message)}>Copy text</DropdownMenuItem>
            <DropdownMenuItem icon={<Link2 size={16} strokeWidth={1.75} />} shortcut={keys("message.copyLink")} onSelect={() => actions.copyLink(message)}>Copy link</DropdownMenuItem>
            {voice.listen && canListen(message) ? <DropdownMenuItem icon={<Headphones size={16} strokeWidth={1.75} />} shortcut={keys("message.listen")} onSelect={() => toggleListen(actions.address, message)}>Listen</DropdownMenuItem> : null}
          </>
        ) : null}
        {message ? <DropdownMenuItem icon={<Braces size={16} strokeWidth={1.75} />} shortcut={keys("message.details")} onSelect={() => actions.openDetails(message)}>Message details</DropdownMenuItem> : null}
        {message && mine && !deleted && !actions.readOnly ? (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem destructive icon={<Trash2 size={16} strokeWidth={1.75} />} shortcut={keys("message.delete")} onSelect={() => actions.requestDelete(message)}>Delete…</DropdownMenuItem>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenuRoot>
  );
}
