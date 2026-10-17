"use client";

/**
 * Settings → Keyboard: every shortcut with its keys, each changeable with Silicon UI's shortcut recorder (it asks
 * before taking keys another shortcut uses, and warns about the browser's own), back to its default one by one or all
 * at once. A filter finds a shortcut by what it does or by its keys. Changes are kept in this browser
 * (`dm:shortcuts:v1`) and apply in every tab at once.
 */
import { Fragment, useMemo, useState } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { SearchField } from "@/components/silicon-ui/search-field/search-field";
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
  const [query, setQuery] = useState("");
  const changed = Object.keys(overrides).length;
  const label = (id: string) => shortcuts.find(entry => entry.id === id)?.label ?? id;

  const groups = useMemo(() => {
    const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    return SHORTCUT_GROUPS.map(group => ({
      group,
      items: shortcuts.filter(shortcut => {
        if (shortcut.group !== group) return false;
        if (!words.length) return true;
        const keys = shortcut.bindings.map(binding => {
          const format = formatBinding(binding, platform);
          return `${format.text} ${format.spoken}`;
        }).join(" ");
        const text = `${shortcut.label} ${shortcut.keywords ?? ""} ${group} ${scopeWords(shortcut.scope)} ${keys}`.toLowerCase();
        return words.every(word => text.includes(word));
      }),
    })).filter(entry => entry.items.length);
  }, [shortcuts, platform, query]);

  return (
    <div className={styles.stack}>
      <p className={styles.text}>
        Choose a shortcut&apos;s keys and press the new ones. Plain keys such as N work outside text fields only; a sequence such as G then S
        can be replaced by a combination. Changes stay in this browser.
      </p>
      <div className={styles.keyFilter}>
        <SearchField label="Find a shortcut" placeholder="Find a shortcut: reply, ⌘K, voice…" value={query} onValueChange={setQuery} autoComplete="off" spellCheck={false} />
      </div>
      {conflicts.length ? (
        <div className={styles.warning} role="alert">
          {conflicts.map(conflict => (
            <p key={conflict.binding}>
              <strong>{formatBinding(conflict.binding, platform).text}</strong> does more than one thing: {conflict.ids.map(label).join(" and ")}. Change one of them.
            </p>
          ))}
        </div>
      ) : null}
      {groups.length ? groups.map(({ group, items }) => (
        <section key={group} className={styles.keyGroup} aria-labelledby={`keys-${group}`}>
          <h3 id={`keys-${group}`} className={styles.keyGroupTitle}>{group}</h3>
          <ul role="list" className={styles.keyRows}>
            {items.map(shortcut => {
              const available = isShortcutAvailable(shortcut.id);
              const others = shortcuts.filter(other => other.id !== shortcut.id).flatMap(other => other.bindings.map(binding => ({ shortcut: binding, label: other.label })));
              const first = shortcut.bindings[0] ?? null;
              const sequence = !!first && first.includes(" ");
              // The recorder holds the first keys; the others work too (? and ⌘/, ↑ and K), so they are shown beside it.
              const also = shortcut.bindings.slice(1);
              const defaults = shortcutDefinition(shortcut.id)?.keys ?? [];
              return (
                <li key={shortcut.id} className={styles.keyRow} data-off={available ? undefined : ""} data-shortcut={shortcut.id}>
                  <div className={styles.keyText}>
                    <span className={styles.rowLabel}>{shortcut.label}</span>
                    <span className={styles.rowDescription}>{available ? scopeWords(shortcut.scope) : shortcut.planned}</span>
                  </div>
                  <div className={styles.keyControl}>
                    {shortcut.fixed ? (
                      <span className={styles.fixed}>
                        {shortcut.bindings.map((binding, index) => <Fragment key={binding}>{index ? <span className={styles.or}>or</span> : null}<Keys binding={binding} /></Fragment>)}
                        <a className={styles.fixedNote} href="#sending">Set in Sending</a>
                      </span>
                    ) : (
                      <>
                        {sequence ? <span className={styles.sequence}><Keys binding={first} /></span> : null}
                        <ShortcutRecorder
                          label={`Keys for ${shortcut.label}`}
                          hideLabel
                          value={sequence ? null : first}
                          placeholder={sequence ? "Use other keys" : "Record keys"}
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
                        {also.length ? (
                          <span className={styles.also}>
                            {also.map(binding => <Fragment key={binding}><span className={styles.or}>or</span><Keys binding={binding} /></Fragment>)}
                          </span>
                        ) : null}
                        {overrides[shortcut.id] ? (
                          <Button size="sm" variant="ghost" onClick={() => setShortcutKeys(shortcut.id, null)} aria-label={`Default keys for ${shortcut.label}`}>Default</Button>
                        ) : null}
                      </>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      )) : (
        <p className={styles.quiet} role="status">No shortcut matches “{query.trim()}”. Try what it does (reply, voice) or its keys (⌘K).</p>
      )}
      <div className={styles.keyFooter}>
        <p className={styles.quiet}>{changed ? `${changed} ${changed === 1 ? "shortcut has" : "shortcuts have"} your own keys.` : "Every shortcut has its default keys."}</p>
        {changed ? <Button variant="secondary" size="sm" onClick={resetAllShortcuts}>Restore every default</Button> : null}
      </div>
    </div>
  );
}
