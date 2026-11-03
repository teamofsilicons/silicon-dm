/**
 * Which shortcut a keydown means, given where the keyboard is: the pure core of the dispatcher (lib/shortcuts/react.tsx).
 *
 * - A shortcut fires only in its scope: anywhere (global), in an open conversation (thread), on a selected message
 *   (message) or in the message field (composer).
 * - A plain key (no ⌘/Ctrl/⌥) never fires while typing in a field, except the field's own (composer) shortcuts.
 * - Sequences ("g s") wait up to 1.5 s for their next key; a key that continues no sequence starts over.
 * - When two scopes both match, the narrower one wins: the message, the field, the conversation, then anywhere.
 */
import type { EffectiveShortcut } from "./bindings";
import { isPlainChord, type Platform } from "./keys";
import type { ShortcutScope } from "./registry";

export const SEQUENCE_TIMEOUT_MS = 1500;

export interface MatchContext {
  /** Scopes active now ("global" always is). */
  scopes: ReadonlySet<ShortcutScope>;
  /** The keyboard is in a text field (the composer counts: its scope says so). */
  inField: boolean;
  platform?: Platform;
}

export interface SequenceState {
  prefix: string[];
  at: number;
}

export const IDLE: SequenceState = { prefix: [], at: 0 };

export interface MatchResult {
  /** The shortcut to run, if the keys completed one. */
  id?: string;
  /** The keys started (or continued) a sequence: swallow this key and wait for the next. */
  pending: boolean;
  state: SequenceState;
}

const PRIORITY: Record<ShortcutScope, number> = { message: 3, composer: 2, thread: 1, global: 0 };

function usable(shortcut: EffectiveShortcut, context: MatchContext, firstChord: string): boolean {
  if (!context.scopes.has(shortcut.scope)) return false;
  if (context.inField && shortcut.scope !== "composer" && isPlainChord(firstChord)) return false;
  return true;
}

/** Text navigation, selection, deletion, clipboard and undo belong to the focused field, including custom bindings. */
export function isTextEditingChord(chord: string, platform: Platform): boolean {
  const parts = chord.split("+");
  const key = parts.pop()!;
  const modified = parts.length > 0;
  if (["left", "right", "home", "end"].includes(key) || (modified && ["up", "down", "backspace", "delete", "pageup", "pagedown", "insert"].includes(key))) return true;
  if (parts.includes("mod") && ["a", "c", "v", "x", "z", "y"].includes(key)) return true;
  if (platform === "mac") {
    // Option also types accents and symbols; Control has macOS's Emacs-style editing commands.
    if (parts.includes("alt") && !parts.includes("mod") && !parts.includes("ctrl")) return true;
    if (parts.includes("ctrl") && !parts.includes("mod") && !parts.includes("alt") && "abdefhklnoptuvwy".includes(key) && key.length === 1) return true;
  }
  return false;
}

/** Feeds one chord (from chordFromEvent) to the matcher. */
export function matchChord(shortcuts: EffectiveShortcut[], state: SequenceState, chord: string, now: number, context: MatchContext): MatchResult {
  if (context.inField && isTextEditingChord(chord, context.platform ?? "mac")) return { pending: false, state: IDLE };
  const prefix = state.prefix.length && now - state.at <= SEQUENCE_TIMEOUT_MS ? state.prefix : [];
  const attempt = (chords: string[]): MatchResult | null => {
    const candidate = chords.join(" ");
    let best: EffectiveShortcut | undefined;
    let continues = false;
    for (const shortcut of shortcuts) {
      for (const binding of shortcut.bindings) {
        const first = binding.split(" ")[0] ?? "";
        if (!usable(shortcut, context, first)) continue;
        if (binding === candidate) {
          if (!best || PRIORITY[shortcut.scope] > PRIORITY[best.scope]) best = shortcut;
        } else if (binding.startsWith(`${candidate} `)) {
          continues = true;
        }
      }
    }
    if (best) return { id: best.id, pending: false, state: IDLE };
    if (continues) return { pending: true, state: { prefix: chords, at: now } };
    return null;
  };
  if (prefix.length) {
    const continued = attempt([...prefix, chord]);
    if (continued) return continued;
  }
  return attempt([chord]) ?? { pending: false, state: IDLE };
}
