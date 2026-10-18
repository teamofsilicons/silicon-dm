/**
 * Keys as the shortcut registry stores them, in Silicon UI's shortcut-recorder format (so a recorded shortcut and a
 * default read the same): a chord is modifiers and one key joined by "+" ("mod+shift+l", "alt+up", "shift+/"), and a
 * binding is one or more chords separated by a space ("g s" is G, then S). `mod` is ⌘ on Apple platforms and Ctrl
 * elsewhere. Keys come from the physical key (KeyboardEvent.code) where layout would change them, so ⌥K is "alt+k",
 * not "˚", and "?" is "shift+/".
 *
 * Pure (no DOM, no React): tests/shortcuts.test.ts runs it under Node.
 */

export type Platform = "mac" | "other";

const MODIFIER_ORDER = ["mod", "ctrl", "alt", "shift", "meta"] as const;
type Modifier = (typeof MODIFIER_ORDER)[number];
const isModifier = (part: string): part is Modifier => (MODIFIER_ORDER as readonly string[]).includes(part);

const CODE_KEYS: Record<string, string> = {
  Comma: ",", Period: ".", Slash: "/", Semicolon: ";", Quote: "'", BracketLeft: "[", BracketRight: "]", Backslash: "\\", Minus: "-", Equal: "=", Backquote: "`",
  Space: "space", Enter: "enter", NumpadEnter: "enter", Escape: "escape", Backspace: "backspace", Delete: "delete", Tab: "tab",
  ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right", Home: "home", End: "end", PageUp: "pageup", PageDown: "pagedown",
};
const MODIFIER_KEYS: Record<string, string> = { Meta: "meta", OS: "meta", Control: "ctrl", Alt: "alt", AltGraph: "alt", Shift: "shift" };

export interface KeyLike {
  key: string;
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/** The physical key of an event ("k", "1", "enter", "up", "/"), or null for keys that never make a shortcut. */
export function keyId(event: Pick<KeyLike, "key" | "code">): string | null {
  const { code, key } = event;
  if (MODIFIER_KEYS[key]) return MODIFIER_KEYS[key]!;
  if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
  if (/^Digit\d$/.test(code)) return code.slice(5);
  if (/^Numpad\d$/.test(code)) return code.slice(6);
  if (CODE_KEYS[code]) return CODE_KEYS[code]!;
  if (/^F\d{1,2}$/.test(key)) return key.toLowerCase();
  if (key === "CapsLock" || key === "Fn" || key === "Dead" || key === "Unidentified" || key === "Process") return null;
  return key.length === 1 ? key.toLowerCase() : null;
}

function splitChord(chord: string) {
  const parts = chord.toLowerCase().split("+").map(part => part.trim()).filter(Boolean);
  const key = parts.filter(part => !isModifier(part)).pop() ?? "";
  return { mods: new Set(parts.filter(isModifier)), key };
}

/** One spelling per chord: on a Mac Command is `mod`, elsewhere Ctrl is; modifiers in a fixed order. */
export function normalizeChord(chord: string, platform: Platform): string {
  const { mods, key } = splitChord(chord);
  if (platform === "mac" && mods.delete("meta")) mods.add("mod");
  if (platform === "other" && mods.delete("ctrl")) mods.add("mod");
  return [...MODIFIER_ORDER.filter(mod => mods.has(mod)), key].filter(Boolean).join("+");
}

/** The chords of a binding ("g s" → ["g", "s"]), normalized. */
export function chordsOf(binding: string, platform: Platform): string[] {
  return binding.trim().split(/\s+/).filter(Boolean).map(chord => normalizeChord(chord, platform));
}

export const normalizeBinding = (binding: string, platform: Platform) => chordsOf(binding, platform).join(" ");

/** The chord a keydown makes, or null while only modifiers are down. */
export function chordFromEvent(event: KeyLike, platform: Platform): string | null {
  const key = keyId(event);
  if (!key || ["meta", "ctrl", "alt", "shift"].includes(key)) return null;
  const mods: string[] = [];
  if (platform === "mac" ? event.metaKey : event.ctrlKey) mods.push("mod");
  if (platform === "mac" && event.ctrlKey) mods.push("ctrl");
  if (event.altKey) mods.push("alt");
  if (event.shiftKey) mods.push("shift");
  if (platform === "other" && event.metaKey) mods.push("meta");
  return normalizeChord([...mods, key].join("+"), platform);
}

/** A chord without ⌘/Ctrl/⌥ (Shift alone allowed): typing it in a field must type, so it never fires there. */
export function isPlainChord(chord: string): boolean {
  const { mods } = splitChord(chord);
  return !mods.has("mod") && !mods.has("ctrl") && !mods.has("alt") && !mods.has("meta");
}

/** Whether a binding is typed like text (its first chord is plain): such shortcuts never fire inside a field. */
export const isPlainBinding = (binding: string) => isPlainChord(binding.trim().split(/\s+/)[0] ?? "");

export interface KeyCap {
  /** What the key cap shows: ⌘, ⇧, K, ↑… */
  label: string;
  /** How assistive technology says it: Command, Shift, K, Up arrow… */
  spoken: string;
}

const MAC_KEYS: Record<string, [string, string]> = { enter: ["↩", "Return"], backspace: ["⌫", "Delete"], delete: ["⌦", "Forward delete"], tab: ["⇥", "Tab"], escape: ["Esc", "Escape"] };
const SHARED_KEYS: Record<string, [string, string]> = {
  space: ["Space", "Space"], enter: ["Enter", "Enter"], escape: ["Esc", "Escape"], backspace: ["Backspace", "Backspace"], delete: ["Del", "Delete"], tab: ["Tab", "Tab"],
  up: ["↑", "Up arrow"], down: ["↓", "Down arrow"], left: ["←", "Left arrow"], right: ["→", "Right arrow"], home: ["Home", "Home"], end: ["End", "End"], pageup: ["PgUp", "Page up"], pagedown: ["PgDn", "Page down"],
  ",": [",", "Comma"], ".": [".", "Period"], "/": ["/", "Slash"], ";": [";", "Semicolon"], "'": ["'", "Quote"], "[": ["[", "Left bracket"], "]": ["]", "Right bracket"], "\\": ["\\", "Backslash"], "-": ["-", "Minus"], "=": ["=", "Equals"], "`": ["`", "Backtick"],
};

/** "shift+/" reads as "?" (and spoken "Question mark"): the key people press, not the one under it. */
const SHIFTED: Record<string, [string, string]> = { "/": ["?", "Question mark"] };

/** Key caps of a chord in platform order: ⌃ ⌥ ⇧ ⌘ on a Mac, Ctrl Alt Shift Win elsewhere. */
export function chordCaps(chord: string, platform: Platform): KeyCap[] {
  const { mods, key } = splitChord(normalizeChord(chord, platform));
  const caps: KeyCap[] = [];
  const shiftedSymbol = mods.has("shift") && SHIFTED[key] && !mods.has("mod") && !mods.has("alt") && !mods.has("ctrl") ? SHIFTED[key] : undefined;
  if (platform === "mac") {
    if (mods.has("ctrl")) caps.push({ label: "⌃", spoken: "Control" });
    if (mods.has("alt")) caps.push({ label: "⌥", spoken: "Option" });
    if (mods.has("shift") && !shiftedSymbol) caps.push({ label: "⇧", spoken: "Shift" });
    if (mods.has("mod") || mods.has("meta")) caps.push({ label: "⌘", spoken: "Command" });
  } else {
    if (mods.has("mod") || mods.has("ctrl")) caps.push({ label: "Ctrl", spoken: "Control" });
    if (mods.has("alt")) caps.push({ label: "Alt", spoken: "Alt" });
    if (mods.has("shift") && !shiftedSymbol) caps.push({ label: "Shift", spoken: "Shift" });
    if (mods.has("meta")) caps.push({ label: "Win", spoken: "Windows" });
  }
  if (shiftedSymbol) caps.push({ label: shiftedSymbol[0], spoken: shiftedSymbol[1] });
  else if (key) {
    const named = (platform === "mac" ? MAC_KEYS[key] : undefined) ?? SHARED_KEYS[key];
    caps.push(named ? { label: named[0], spoken: named[1] } : { label: key.toUpperCase(), spoken: key.toUpperCase() });
  }
  return caps;
}

/** "⌘⇧L" / "Ctrl+Shift+L", "G then S", with the spoken form ("Command Shift L", "G, then S"). */
export function formatBinding(binding: string, platform: Platform): { text: string; spoken: string } {
  const chords = chordsOf(binding, platform).map(chord => chordCaps(chord, platform));
  const text = chords.map(caps => caps.map(cap => cap.label).join(platform === "mac" ? "" : "+")).join(" then ");
  const spoken = chords.map(caps => caps.map(cap => cap.spoken).join(" ")).join(", then ");
  return { text, spoken };
}

/**
 * The value of `aria-keyshortcuts` for a binding ("Meta+Shift+L", "Shift+?"), or null for a sequence (the attribute
 * has no way to say "then").
 */
export function ariaKeyShortcut(binding: string, platform: Platform): string | null {
  const chords = chordsOf(binding, platform);
  if (chords.length !== 1) return null;
  const { mods, key } = splitChord(chords[0]!);
  const names: string[] = [];
  if (mods.has("ctrl") || (platform === "other" && mods.has("mod"))) names.push("Control");
  if (mods.has("alt")) names.push("Alt");
  if (mods.has("shift")) names.push("Shift");
  if ((platform === "mac" && mods.has("mod")) || mods.has("meta")) names.push("Meta");
  const keyName: Record<string, string> = { up: "ArrowUp", down: "ArrowDown", left: "ArrowLeft", right: "ArrowRight", enter: "Enter", escape: "Escape", space: "Space", backspace: "Backspace", delete: "Delete", tab: "Tab", end: "End", home: "Home", pageup: "PageUp", pagedown: "PageDown" };
  names.push(keyName[key] ?? (key.length === 1 ? key.toUpperCase() : key));
  return names.join("+");
}

/** The platform from a user agent hint: Apple devices use ⌘. */
export function platformOf(hint: string | undefined): Platform {
  return hint && /mac|iphone|ipad|ipod/i.test(hint) ? "mac" : "other";
}
