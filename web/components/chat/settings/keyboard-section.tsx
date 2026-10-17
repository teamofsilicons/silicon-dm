"use client";

/**
 * Settings → Keyboard: every shortcut with its keys, each changeable with Silicon UI's shortcut recorder (it asks
 * before taking keys another shortcut uses, and warns about the browser's own), reset one by one or all at once.
 * Changes are kept in this browser (`dm:shortcuts:v1`) and apply in every tab at once.
 */
import { Fragment } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { ShortcutRecorder } from "@/components/silicon-ui/shortcut-recorder/shortcut-recorder";
import { formatBinding, normalizeBinding } from "@/lib/shortcuts/keys";
import { isShortcutAvailable, useShortcutAvailability, useShortcuts } from "@/lib/shortcuts/react";
import { scopeWords, SHORTCUT_GROUPS, shortcutDefinition } from "@/lib/shortcuts/registry";
import { resetAllShortcuts, setShortcutKeys, useShortcutOverrides } from "@/lib/shortcuts/store";
import { Keys } from "../keyboard/keys";
import styles from "./settings.module.css";

export function KeyboardSection() {
  const { shortcuts, platform, conflicts } = useShortcuts();
  const overrides = useShortcutOverrides();
  useShortcutAvailability();
  const changed = Object.keys(overrides).length;
  const label = (id: string) => shortcuts.find(entry => entry.id === id)?.label ?? id;
  return (
    <div className={styles.stack}>
      <p className={styles.text}>
        Press a shortcut&apos;s keys to change it. Plain keys (like N) only work outside text fields; a sequence such as G then S
        can be replaced by a combination. Changes stay in this browser.
      </p>
      {conflicts.length ? (
        <div className={styles.warning} role="alert">
          {conflicts.map(conflict => (
            <p key={conflict.binding}>
              <strong>{formatBinding(conflict.binding, platform).text}</strong> does more than one thing: {conflict.ids.map(label).join(" and ")}. Change one of them.
            </p>
          ))}
        </div>
      ) : null}
      {SHORTCUT_GROUPS.map(group => {
        const items = shortcuts.filter(shortcut => shortcut.group === group);
        return (
          <section key={group} className={styles.keyGroup} aria-labelledby={`keys-${group}`}>
            <h3 id={`keys-${group}`} className={styles.keyGroupTitle}>{group}</h3>
            <ul role="list" className={styles.keyRows}>
              {items.map(shortcut => {
                const available = isShortcutAvailable(shortcut.id);
                const others = shortcuts.filter(other => other.id !== shortcut.id).flatMap(other => other.bindings.map(binding => ({ shortcut: binding, label: other.label })));
                const first = shortcut.bindings[0] ?? null;
                const sequence = !!first && first.includes(" ");
                const defaults = shortcutDefinition(shortcut.id)?.keys ?? [];
                return (
                  <li key={shortcut.id} className={styles.keyRow} data-off={available ? undefined : ""}>
                    <div className={styles.keyText}>
                      <span className={styles.rowLabel}>{shortcut.label}</span>
                      <span className={styles.rowDescription}>{available ? scopeWords(shortcut.scope) : shortcut.planned}</span>
                    </div>
                    <div className={styles.keyControl}>
                      {shortcut.fixed ? (
                        <span className={styles.fixed}>
                          {shortcut.bindings.map((binding, index) => <Fragment key={binding}>{index ? <span className={styles.or}>or</span> : null}<Keys binding={binding} /></Fragment>)}
                          <span className={styles.fixedNote}>Set in Sending</span>
                        </span>
                      ) : (
                        <>
                          {sequence ? <span className={styles.sequence}><Keys binding={first} /></span> : null}
                          <ShortcutRecorder
                            label={`Keys for ${shortcut.label}`}
                            hideLabel
                            value={sequence ? null : first}
                            placeholder={sequence ? "Replace with keys" : "Record keys"}
                            resetValue={sequence ? null : defaults[0] ?? null}
                            bindings={others}
                            requireModifier={false}
                            platform={platform}
                            onValueChange={(value, details) => {
                              const rest = shortcut.bindings.slice(1);
                              // Cleared: the other keys stay (none left switches the shortcut off).
                              if (value === null) setShortcutKeys(shortcut.id, rest);
                              else setShortcutKeys(shortcut.id, [value, ...rest.filter(binding => normalizeBinding(binding, platform) !== normalizeBinding(value, platform))]);
                              if (details.replaced) {
                                const taken = shortcuts.find(other => other.label === details.replaced?.label);
                                if (taken) setShortcutKeys(taken.id, taken.bindings.filter(binding => normalizeBinding(binding, platform) !== normalizeBinding(details.replaced!.shortcut, platform)));
                              }
                            }}
                          />
                          {overrides[shortcut.id] ? (
                            <Button size="sm" variant="ghost" onClick={() => setShortcutKeys(shortcut.id, null)}>Default</Button>
                          ) : null}
                        </>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          </section>
        );
      })}
      <div className={styles.keyFooter}>
        <p className={styles.quiet}>{changed ? `${changed} ${changed === 1 ? "shortcut is" : "shortcuts are"} yours.` : "Every shortcut has its default keys."}</p>
        {changed ? <Button variant="secondary" size="sm" onClick={resetAllShortcuts}>Restore every default</Button> : null}
      </div>
    </div>
  );
}
