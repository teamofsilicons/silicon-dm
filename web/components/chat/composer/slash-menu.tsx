"use client";

/**
 * The slash commands' list (./slash.ts), above the field while its text starts with "/". The keyboard stays in the
 * field (the list is its `aria-activedescendant`): ↑/↓ move, Tab or Enter runs, Esc closes the list and leaves the text.
 * A click runs a command too (the field keeps the keyboard).
 */
import { useShortcutText } from "@/lib/shortcuts/react";
import type { SlashOption } from "./slash";
import styles from "./composer.module.css";

export const optionId = (list: string, option: SlashOption) => `${list}-${option.command.name}`;

export function SlashMenu({ id, options, active, onRun, onHover }: { id: string; options: SlashOption[]; active: number; onRun: (index: number) => void; onHover: (index: number) => void }) {
  const keys = useShortcutText();
  return (
    <div className={styles.slashMenu} data-sq="surface">
      <div role="listbox" id={id} aria-label="Commands" className={styles.slashList}>
        {options.map((option, index) => {
          const shortcut = option.command.shortcut ? keys(option.command.shortcut) : "";
          return (
            <div
              key={option.command.name}
              id={optionId(id, option)}
              role="option"
              aria-selected={index === active}
              className={styles.slashOption}
              data-sq="surface"
              onMouseDown={event => event.preventDefault()}
              onMouseMove={() => onHover(index)}
              onClick={() => onRun(index)}
            >
              <span className={styles.slashName}>
                /{option.command.name}
                {option.argument ? <span className={styles.slashArgument}> {option.argument}</span> : option.command.argument === "optional" ? <span className={styles.slashHint}> {option.command.usage.split(" ").slice(1).join(" ")}</span> : null}
              </span>
              <span className={styles.slashWhat}>{option.command.description}</span>
              {shortcut ? <kbd className={styles.slashKeys}>{shortcut}</kbd> : null}
            </div>
          );
        })}
      </div>
      <p className={styles.slashFoot} aria-hidden="true">↑↓ choose · Tab or Enter runs · Esc keeps it as text</p>
    </div>
  );
}
