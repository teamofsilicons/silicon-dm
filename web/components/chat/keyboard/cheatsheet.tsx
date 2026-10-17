"use client";

/**
 * The keyboard cheatsheet (? or ⌘/): every shortcut of the registry with its current keys, grouped as the registry
 * groups them, searchable by name or key. A shortcut whose feature does not work here (no speech on this DM service, no
 * microphone) is shown off, with the reason.
 */
import Link from "next/link";
import { useMemo, useState } from "react";
import { Search } from "lucide-react";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { formatBinding } from "@/lib/shortcuts/keys";
import { shortcutOffReason, useShortcutAvailability, useShortcuts } from "@/lib/shortcuts/react";
import { scopeWords, SHORTCUT_GROUPS } from "@/lib/shortcuts/registry";
import { closeDialog } from "../dialogs/store";
import { Keys } from "./keys";
import styles from "./keyboard.module.css";

export function Cheatsheet({ open }: { open: boolean }) {
  const { shortcuts, platform } = useShortcuts();
  useShortcutAvailability();
  const [query, setQuery] = useState("");
  const groups = useMemo(() => {
    const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    return SHORTCUT_GROUPS.map(group => ({
      group,
      items: shortcuts.filter(shortcut => {
        if (shortcut.group !== group) return false;
        if (!words.length) return true;
        const keys = shortcut.bindings.map(binding => `${formatBinding(binding, platform).text} ${formatBinding(binding, platform).spoken}`).join(" ");
        const text = `${shortcut.label} ${shortcut.keywords ?? ""} ${group} ${keys}`.toLowerCase();
        return words.every(word => text.includes(word));
      }),
    })).filter(entry => entry.items.length);
  }, [shortcuts, platform, query]);

  return (
    <Dialog open={open} onOpenChange={next => { if (!next) { closeDialog("shortcuts"); setQuery(""); } }}>
      <DialogContent title="Keyboard shortcuts" description="Plain keys work outside text fields. ⌘ is Ctrl on Windows and Linux." className={styles.cheatsheet}
        onOpenAutoFocus={event => { event.preventDefault(); document.getElementById("cheatsheet-search")?.focus(); }}>
        <div className={styles.search} data-sq="surface">
          <Search size={16} strokeWidth={1.75} aria-hidden="true" />
          <input id="cheatsheet-search" type="search" value={query} onChange={event => setQuery(event.target.value)} placeholder="Find a shortcut" aria-label="Find a shortcut" autoComplete="off" spellCheck={false} />
        </div>
        <div className={styles.sheet} tabIndex={0} role="region" aria-label="Every shortcut">
          {groups.map(({ group, items }) => (
            <section key={group} className={styles.group} aria-labelledby={`cheatsheet-${group}`}>
              <h3 id={`cheatsheet-${group}`} className={styles.groupTitle}>{group}</h3>
              <ul role="list" className={styles.rows}>
                {items.map(shortcut => {
                  const off = shortcutOffReason(shortcut.id);
                  return (
                    <li key={shortcut.id} className={styles.row} data-off={off === null ? undefined : ""}>
                      <span className={styles.rowLabel}>
                        <span>{shortcut.label}</span>
                        {off === null ? null : <span className={styles.rowReason}>{off}</span>}
                        <span className="sr-only">, {scopeWords(shortcut.scope).toLowerCase()}</span>
                      </span>
                      <span className={styles.rowKeys}>
                        {shortcut.bindings.length ? shortcut.bindings.map((binding, index) => (
                          <span key={binding} className={styles.alternative}>
                            {index ? <span className={styles.or}>or</span> : null}
                            <Keys binding={binding} />
                          </span>
                        )) : <span className={styles.none}>Off</span>}
                      </span>
                    </li>
                  );
                })}
              </ul>
            </section>
          ))}
          {groups.length ? null : <p className={styles.nothing}>No shortcut matches “{query.trim()}”.</p>}
        </div>
        <p className={styles.footnote}>
          Change any of them in <Link href="/settings#keyboard" onClick={() => closeDialog("shortcuts")}>Settings → Keyboard</Link>.
        </p>
      </DialogContent>
    </Dialog>
  );
}
