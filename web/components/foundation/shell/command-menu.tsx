"use client";

/**
 * The ⌘K command palette: Silicon UI's command-palette block in a modal layer. Commands come from the registry
 * (lib/commands.ts): the chat's conversations, actions and views (components/chat/shell/global-commands.tsx) plus
 * whatever the current page adds. A command tied to a keyboard shortcut shows its current keys. What a query finds is
 * ranked by lib/command-rank.ts (a conversation named by what was typed comes before an action that merely contains
 * the letters), and the best match is highlighted, so Enter always runs what is on screen.
 */
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { useCallback, useMemo } from "react";
import { CommandPalette, type CommandItem } from "@/components/silicon-ui/command-palette/command-palette";
import { returnFocusTo, useLayerOpener } from "@/components/foundation/return-focus";
import { useOpenAddress } from "@/components/chat/sidebar/conversations-pane";
import { rankCommands } from "@/lib/command-rank";
import { closeCommandPalette, setCommandPaletteOpen, setCommandPaletteQuery, useCommandPaletteOpen, useCommands } from "@/lib/commands";
import { useShortcutText } from "@/lib/shortcuts/react";
import styles from "./command-menu.module.css";

export function CommandMenu() {
  const open = useCommandPaletteOpen();
  // Opened by ⌘K or a button (no Radix Trigger): closing returns focus to what had it (lib/return-focus.ts).
  const opener = useLayerOpener(open);
  const commands = useCommands();
  const keys = useShortcutText();
  const openAddress = useOpenAddress();
  const items: CommandItem[] = commands.map(({ id, label, description, group, keywords, icon, shortcut, shortcutId }) => ({ id, label, description, group, keywords, icon, shortcut: shortcut ?? (shortcutId ? keys(shortcutId) || undefined : undefined) }));
  // What ranks an item and is not drawn: its other names, its people, and whether it is a conversation.
  const hints = useMemo(() => new Map(commands.map(command => [command.id, { names: command.names, members: command.members, conversation: command.conversation }])), [commands]);
  const current = openAddress ? `open.${openAddress}` : null;
  const filter = useCallback(
    (list: CommandItem[], query: string) => rankCommands(list.map(item => ({ ...item, ...hints.get(item.id) })), query, { current }),
    [hints, current],
  );
  const run = (item: CommandItem) => {
    closeCommandPalette();
    const command = commands.find(entry => entry.id === item.id);
    // Run after the dialog has started closing, so focus returns before the command moves it (navigation, a dialog).
    if (command) requestAnimationFrame(() => command.run());
  };
  return (
    <DialogPrimitive.Root open={open} onOpenChange={setCommandPaletteOpen}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className={styles.paletteOverlay} />
        <div className={styles.palettePositioner}>
          <DialogPrimitive.Content className={styles.palettePanel} aria-describedby={undefined} onCloseAutoFocus={returnFocusTo(opener)}>
            <DialogPrimitive.Title className="sr-only">Search conversations and actions</DialogPrimitive.Title>
            {open ? (
              <CommandPalette
                items={items}
                filter={filter}
                onQueryChange={setCommandPaletteQuery}
                emptyLabel="No conversation or action matches"
                placeholder="Search conversations and actions"
                label="Search conversations and actions"
                autoFocus
                onClose={closeCommandPalette}
                onSelect={run}
              />
            ) : null}
          </DialogPrimitive.Content>
        </div>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
