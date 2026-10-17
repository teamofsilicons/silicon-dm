"use client";

/**
 * The keyboard at run time. <ShortcutsProvider> (in the chat shell) listens to every keydown once, works out where
 * the keyboard is (a field, a selected message, the composer, an open conversation), matches the effective bindings
 * (lib/shortcuts/match.ts) and runs the handler a component attached:
 *
 *   useShortcut("conversation.new", () => openNewConversation());
 *   useShortcut("composer.editLast", () => { if (text) return false; editLast(); });   // false: let the key through
 *   useShortcutScope("thread", true);                                                   // while a conversation is open
 *   const keys = useShortcutKeys("details.toggle");                                     // "⌘." for a tooltip
 *
 * The most recently attached handler of an id runs (a dialog's own handler wins over the page's). Keys inside an open
 * dialog, menu or list belong to that layer and are left alone.
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useSyncExternalStore, type ReactNode } from "react";
import { useChatSettings } from "@/lib/chat/settings";
import { effectiveShortcuts, findConflicts, type Conflict, type EffectiveShortcut } from "./bindings";
import { ariaKeyShortcut, chordFromEvent, formatBinding, platformOf, type Platform } from "./keys";
import { IDLE, matchChord, type SequenceState } from "./match";
import { shortcutDefinition, type ShortcutScope } from "./registry";
import { useShortcutOverrides } from "./store";

export type ShortcutHandler = (event: KeyboardEvent) => void | boolean;

/* Handlers ---------------------------------------------------------------------------------------------------------- */

interface Registration {
  id: string;
  handler: { current: ShortcutHandler };
  enabled: { current: boolean };
  order: number;
}

let order = 0;
const registrations = new Map<string, Registration[]>();
const availabilityListeners = new Set<() => void>();
const provided = new Set<string>();
let availabilityVersion = 0;

function emitAvailability() {
  availabilityVersion += 1;
  for (const listener of availabilityListeners) listener();
}

/** Marks shortcuts whose feature has arrived (a `planned` one stops showing as off). Phase-2 modules call this. */
export function provideShortcuts(ids: string[]): void {
  let changed = false;
  for (const id of ids) if (!provided.has(id)) {
    provided.add(id);
    changed = true;
  }
  if (changed) emitAvailability();
}

/** Whether a shortcut's feature is here (not `planned`, or provided). */
export function isShortcutAvailable(id: string): boolean {
  const definition = shortcutDefinition(id);
  return !!definition && (!definition.planned || provided.has(id));
}

/** Re-renders when shortcuts become available. */
export function useShortcutAvailability(): number {
  return useSyncExternalStore(
    listener => {
      availabilityListeners.add(listener);
      return () => availabilityListeners.delete(listener);
    },
    () => availabilityVersion,
    () => 0,
  );
}

/** Runs `handler` when the shortcut's keys are pressed in its scope, while `enabled`. */
export function useShortcut(id: string, handler: ShortcutHandler, enabled = true): void {
  const handlerRef = useRef(handler);
  const enabledRef = useRef(enabled);
  useEffect(() => {
    handlerRef.current = handler;
    enabledRef.current = enabled;
  });
  useEffect(() => {
    const registration: Registration = { id, handler: handlerRef, enabled: enabledRef, order: ++order };
    registrations.set(id, [...(registrations.get(id) ?? []), registration]);
    return () => {
      const rest = (registrations.get(id) ?? []).filter(entry => entry !== registration);
      if (rest.length) registrations.set(id, rest);
      else registrations.delete(id);
    };
  }, [id]);
}

function handlerFor(id: string): ShortcutHandler | null {
  const list = registrations.get(id);
  if (!list) return null;
  for (let index = list.length - 1; index >= 0; index--) {
    const entry = list[index]!;
    if (entry.enabled.current) return entry.handler.current;
  }
  return null;
}

/** Runs a shortcut's action as if its keys were pressed (the palette and the menus use this). Returns whether one ran. */
export function runShortcut(id: string): boolean {
  const handler = handlerFor(id);
  if (!handler) return false;
  handler(new KeyboardEvent("keydown"));
  return true;
}

/** Whether a component has attached an enabled handler to this shortcut right now. */
export const hasShortcutHandler = (id: string) => handlerFor(id) !== null;

/* Scopes ------------------------------------------------------------------------------------------------------------ */

const scopeCounts: Record<ShortcutScope, number> = { global: 1, thread: 0, message: 0, composer: 0 };

/** Turns a scope on while mounted (the thread view turns "thread" on). Message and composer follow focus by themselves. */
export function useShortcutScope(scope: Exclude<ShortcutScope, "global">, active: boolean): void {
  useEffect(() => {
    if (!active) return;
    scopeCounts[scope] += 1;
    return () => {
      scopeCounts[scope] -= 1;
    };
  }, [scope, active]);
}

/* Platform ---------------------------------------------------------------------------------------------------------- */

const subscribeNothing = () => () => undefined;
const detectPlatform = (): Platform => {
  const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
  return platformOf(nav.userAgentData?.platform || nav.platform || nav.userAgent);
};

/** ⌘ on Apple devices, Ctrl elsewhere. Server renders assume Apple and settle on the first client render. */
export function usePlatform(): Platform {
  return useSyncExternalStore(subscribeNothing, detectPlatform, () => "mac" as Platform);
}

/* Provider ---------------------------------------------------------------------------------------------------------- */

interface ShortcutsValue {
  platform: Platform;
  shortcuts: EffectiveShortcut[];
  conflicts: Conflict[];
}

const ShortcutsContext = createContext<ShortcutsValue | null>(null);

const FIELD = 'input:not([type="checkbox"]):not([type="radio"]):not([type="button"]):not([type="submit"]):not([type="reset"]):not([type="range"]):not([type="color"]):not([type="file"]), textarea, select, [contenteditable]:not([contenteditable="false"])';
// A layer that is closing (data-state="closed" while it animates out) no longer owns the keys.
const LAYER = '[role="dialog"]:not([data-state="closed"]), [role="alertdialog"]:not([data-state="closed"]), [role="menu"]:not([data-state="closed"]), [role="listbox"], [data-shortcuts="off"]';

export function ShortcutsProvider({ children }: { children: ReactNode }) {
  const platform = usePlatform();
  const overrides = useShortcutOverrides();
  const settings = useChatSettings();
  const shortcuts = useMemo(() => effectiveShortcuts(overrides, { platform, sendWith: settings.sendWith }), [overrides, platform, settings.sendWith]);
  const conflicts = useMemo(() => findConflicts(shortcuts, platform), [shortcuts, platform]);
  const state = useRef<SequenceState>(IDLE);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
      const target = event.target instanceof Element ? event.target : null;
      // An open dialog, menu or list owns its keys (Escape closes it, arrows move in it).
      if (target?.closest(LAYER) && !target.closest("[data-shortcuts='on']")) return;
      const chord = chordFromEvent(event, platform);
      if (!chord) return;
      const inField = !!target?.closest(FIELD);
      const scopes = new Set<ShortcutScope>(["global"]);
      if (scopeCounts.thread > 0) scopes.add("thread");
      // A message has the keyboard when it was reached by keyboard (focus-visible), not when a click focused it.
      const message = !inField ? target?.closest<HTMLElement>("[data-message-row]") : null;
      if (target?.closest("[data-composer-field]")) scopes.add("composer");
      else if (message && message.matches(":focus-visible")) scopes.add("message");
      const result = matchChord(shortcuts, state.current, chord, Date.now(), { scopes, inField });
      state.current = result.state;
      if (result.pending) {
        event.preventDefault();
        return;
      }
      if (!result.id) return;
      const handler = handlerFor(result.id);
      if (!handler) return;
      if (handler(event) === false) return;
      event.preventDefault();
      event.stopPropagation();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [platform, shortcuts]);

  const value = useMemo(() => ({ platform, shortcuts, conflicts }), [platform, shortcuts, conflicts]);
  return <ShortcutsContext.Provider value={value}>{children}</ShortcutsContext.Provider>;
}

/** The effective shortcuts, the platform and the conflicts (Settings → Keyboard, the cheatsheet). */
export function useShortcuts(): ShortcutsValue {
  const value = useContext(ShortcutsContext);
  const platform = usePlatform();
  const overrides = useShortcutOverrides();
  const settings = useChatSettings();
  // Outside the provider (the public pages never need it), compute the same from the stores.
  const fallback = useMemo(() => {
    const shortcuts = effectiveShortcuts(overrides, { platform, sendWith: settings.sendWith });
    return { platform, shortcuts, conflicts: findConflicts(shortcuts, platform) };
  }, [overrides, platform, settings.sendWith]);
  return value ?? fallback;
}

export interface ShortcutKeys {
  /** The first binding as shown ("⌘⇧L", "G then S"), or "" when the shortcut has no keys. */
  text: string;
  /** How it is spoken ("Command Shift L"). */
  spoken: string;
  /** For `aria-keyshortcuts` (undefined for sequences and none). */
  aria: string | undefined;
  /** Every binding, each as shown. */
  all: string[];
  /** The first binding in the registry's format, for <ShortcutKeys>. */
  binding: string | null;
}

/** The keys of one shortcut for the current platform, for tooltips, menus and aria-keyshortcuts. */
export function useShortcutKeys(id: string): ShortcutKeys {
  const { shortcuts, platform } = useShortcuts();
  const shortcut = shortcuts.find(entry => entry.id === id);
  const first = shortcut?.bindings[0] ?? null;
  const formatted = first ? formatBinding(first, platform) : { text: "", spoken: "" };
  return {
    text: formatted.text,
    spoken: formatted.spoken,
    aria: first ? ariaKeyShortcut(first, platform) ?? undefined : undefined,
    all: (shortcut?.bindings ?? []).map(binding => formatBinding(binding, platform).text),
    binding: first,
  };
}

/** A stable callback that formats any shortcut's first binding (for lists built in a loop). */
export function useShortcutText(): (id: string) => string {
  const { shortcuts, platform } = useShortcuts();
  return useCallback((id: string) => {
    const first = shortcuts.find(entry => entry.id === id)?.bindings[0];
    return first ? formatBinding(first, platform).text : "";
  }, [shortcuts, platform]);
}
