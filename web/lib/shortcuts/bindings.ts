/**
 * Effective bindings: the defaults of lib/shortcuts/registry.ts with the Carbon's own keys (Settings → Keyboard) on
 * top, and the conflicts between them. Pure, so tests/shortcuts.test.ts checks it; lib/shortcuts/store.ts keeps the
 * overrides in localStorage (`dm:shortcuts:v1`).
 */
import { chordsOf, isPlainBinding, normalizeBinding, type Platform } from "./keys";
import { scopesOverlap, SHORTCUTS, type ShortcutDefinition } from "./registry";

/** The Carbon's own keys, by shortcut id. An empty list switches a shortcut off. */
export type Overrides = Record<string, string[]>;

export interface EffectiveShortcut extends ShortcutDefinition {
  /** The keys in use now (overridden or default). */
  bindings: string[];
  overridden: boolean;
}

export interface BindingContext {
  platform: Platform;
  /** Settings → Sending: Enter sends (and Shift+Enter is a new line), or ⌘/Ctrl+Enter sends (and Enter is a new line). */
  sendWith: "enter" | "mod-enter";
}

/** Every shortcut with the keys it has now. */
export function effectiveShortcuts(overrides: Overrides, context: BindingContext, definitions: ShortcutDefinition[] = SHORTCUTS): EffectiveShortcut[] {
  return definitions.map(definition => {
    const own = overrides[definition.id];
    let bindings = own ?? definition.keys;
    if (!own && definition.id === "composer.send" && context.sendWith === "mod-enter") bindings = ["mod+enter"];
    if (!own && definition.id === "composer.newline" && context.sendWith === "mod-enter") bindings = ["enter", "shift+enter"];
    return { ...definition, bindings: bindings.map(binding => normalizeBinding(binding, context.platform)).filter(Boolean), overridden: !!own };
  });
}

export interface Conflict {
  /** The keys two or more shortcuts share (or that start another's sequence). */
  binding: string;
  ids: string[];
}

/**
 * Keys that do two things where both shortcuts can be active: the same keys, or keys that begin another shortcut's
 * sequence (G would fire before G then S could finish). A plain key anywhere never meets the message field, where
 * plain keys type instead.
 */
export function findConflicts(shortcuts: EffectiveShortcut[], platform: Platform): Conflict[] {
  const conflicts = new Map<string, Set<string>>();
  const entries = shortcuts.flatMap(shortcut => shortcut.bindings.map(binding => ({ shortcut, binding, chords: chordsOf(binding, platform) })));
  for (let i = 0; i < entries.length; i++) {
    for (let j = i + 1; j < entries.length; j++) {
      const a = entries[i]!;
      const b = entries[j]!;
      if (a.shortcut.id === b.shortcut.id) continue;
      if (!scopesOverlap(a.shortcut.scope, b.shortcut.scope)) continue;
      const plainVsField = (x: typeof a, y: typeof a) => x.shortcut.scope === "global" && y.shortcut.scope === "composer" && isPlainBinding(x.binding);
      if (plainVsField(a, b) || plainVsField(b, a)) continue;
      const same = a.chords.join(" ") === b.chords.join(" ");
      const shorter = a.chords.length <= b.chords.length ? a : b;
      const longer = shorter === a ? b : a;
      const prefix = !same && shorter.chords.every((chord, index) => longer.chords[index] === chord);
      if (!same && !prefix) continue;
      const key = shorter.chords.join(" ");
      const set = conflicts.get(key) ?? new Set<string>();
      set.add(a.shortcut.id);
      set.add(b.shortcut.id);
      conflicts.set(key, set);
    }
  }
  return [...conflicts.entries()].map(([binding, ids]) => ({ binding, ids: [...ids] }));
}

/** Overrides read back from storage: only known ids and string bindings survive. */
export function cleanOverrides(value: unknown, definitions: ShortcutDefinition[] = SHORTCUTS): Overrides {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const known = new Set(definitions.map(definition => definition.id));
  const out: Overrides = {};
  for (const [id, bindings] of Object.entries(value as Record<string, unknown>)) {
    if (!known.has(id) || !Array.isArray(bindings)) continue;
    out[id] = bindings.filter((binding): binding is string => typeof binding === "string" && binding.trim().length > 0 && binding.length < 64).slice(0, 4);
  }
  return out;
}
