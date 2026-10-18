"use client";

/**
 * The composer's + menu (Silicon UI's dropdown menu): Upload a file (⌘⇧U), Add a link, GIF. When this website cannot
 * upload, the first item is off and the menu says why, right under it (never only in a tooltip); links and GIFs work
 * either way.
 *
 * The menu is not modal: the field and the tray stay readable and reachable around it (a modal menu hides the whole
 * page from assistive technology while it is open), and a click anywhere else closes it. Add a link and GIF open once
 * the menu has closed (in its onCloseAutoFocus), so the field or the picker they open keeps the keyboard. Upload a file
 * opens the file chooser right away, while the click still counts as the Carbon's own gesture.
 */
import { useRef } from "react";
import { ImagePlay, Link2, Paperclip, Plus } from "lucide-react";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuRoot, DropdownMenuTrigger } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { useShortcutText } from "@/lib/shortcuts/react";
import { IconAction } from "../common/icon-action";
import styles from "./composer.module.css";

export interface AttachMenuProps {
  /** Why files cannot be uploaded here, or null. */
  uploadsOff: string | null;
  onUpload: () => void;
  onLink: () => void;
  onGif: () => void;
}

export function AttachMenu({ uploadsOff, onUpload, onLink, onGif }: AttachMenuProps) {
  const keys = useShortcutText();
  // What to open once the menu is gone (Add a link, GIF); they take the keyboard themselves.
  const next = useRef<(() => void) | null>(null);
  const icon = { size: 16, strokeWidth: 1.75 } as const;
  return (
    <DropdownMenuRoot modal={false} onOpenChange={open => { if (open) next.current = null; }}>
      <DropdownMenuTrigger asChild>
        <IconAction label="Add a file, a link or a GIF" icon={<Plus strokeWidth={1.75} aria-hidden="true" />} tooltipSide="top" data-composer-attach="" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        side="top"
        align="start"
        className={styles.attachMenu}
        onCloseAutoFocus={event => {
          const run = next.current;
          next.current = null;
          if (!run) return;
          event.preventDefault();
          run();
        }}
      >
        <DropdownMenuItem icon={<Paperclip {...icon} />} shortcut={uploadsOff ? undefined : keys("composer.upload")} disabled={!!uploadsOff} onSelect={onUpload}>
          Upload a file
        </DropdownMenuItem>
        {uploadsOff ? <DropdownMenuLabel className={styles.attachNote}>{uploadsOff}</DropdownMenuLabel> : null}
        <DropdownMenuItem icon={<Link2 {...icon} />} onSelect={() => { next.current = onLink; }}>
          Add a link
        </DropdownMenuItem>
        <DropdownMenuItem icon={<ImagePlay {...icon} />} onSelect={() => { next.current = onGif; }}>
          GIF
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenuRoot>
  );
}
