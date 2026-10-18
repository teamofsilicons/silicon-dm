"use client";

/**
 * Key caps for a binding, in Silicon UI's Kbd: one cap per key, "then" between the steps of a sequence (G then S), and
 * a spoken label for assistive technology. <ShortcutHint id> shows a registry shortcut's current keys (or nothing
 * when it has none), for tooltips and menus.
 */
import { ShortcutKbd } from "@/components/silicon-ui/shortcut-recorder/shortcut-recorder";
import { chordCaps, chordsOf, formatBinding } from "@/lib/shortcuts/keys";
import { usePlatform, useShortcutKeys } from "@/lib/shortcuts/react";
import styles from "./keyboard.module.css";

export function Keys({ binding, size = "sm", className }: { binding: string; size?: "sm" | "md"; className?: string }) {
  const platform = usePlatform();
  const chords = chordsOf(binding, platform);
  return (
    <span className={[styles.keys, className].filter(Boolean).join(" ")} role="img" aria-label={formatBinding(binding, platform).spoken}>
      {chords.map((chord, index) => (
        <span key={`${index}-${chord}`} className={styles.chord} aria-hidden="true">
          {index ? <span className={styles.then}>then</span> : null}
          {chordCaps(chord, platform).map((cap, at) => <ShortcutKbd key={at} size={size}>{cap.label}</ShortcutKbd>)}
        </span>
      ))}
    </span>
  );
}

/** A registry shortcut's current keys (the first binding), or nothing when it has none. */
export function ShortcutHint({ id, size = "sm", className }: { id: string; size?: "sm" | "md"; className?: string }) {
  const keys = useShortcutKeys(id);
  return keys.binding ? <Keys binding={keys.binding} size={size} className={className} /> : null;
}
