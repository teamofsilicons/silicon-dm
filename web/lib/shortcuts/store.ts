"use client";

/**
 * The Carbon's own keys (Settings → Keyboard), kept in this browser (`dm:shortcuts:v1`) and the same in every tab.
 * Only changes are stored: a shortcut without an entry uses its default (lib/shortcuts/registry.ts).
 */
import { useSyncExternalStore } from "react";
import { onStorage, readJson, writeJson } from "@/lib/chat/storage";
import { cleanOverrides, type Overrides } from "./bindings";
import { shortcutDefinition } from "./registry";

export const SHORTCUTS_STORAGE_KEY = "dm:shortcuts:v1";
const EMPTY: Overrides = {};
const listeners = new Set<() => void>();
let current: Overrides | null = null;
let watching = false;

const load = () => cleanOverrides(readJson<unknown>(SHORTCUTS_STORAGE_KEY, {}));

function snapshot(): Overrides {
  if (!current) current = load();
  return current;
}

function emit() {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  if (!watching && typeof window !== "undefined") {
    watching = true;
    onStorage(SHORTCUTS_STORAGE_KEY, () => {
      current = load();
      emit();
    });
  }
  return () => listeners.delete(listener);
}

export function useShortcutOverrides(): Overrides {
  return useSyncExternalStore(subscribe, snapshot, () => EMPTY);
}

export const shortcutOverrides = (): Overrides => snapshot();

/** Sets a shortcut's keys (an empty list switches it off), or `null` to go back to its default. */
export function setShortcutKeys(id: string, bindings: string[] | null): void {
  if (!shortcutDefinition(id)) return;
  const next = { ...snapshot() };
  if (bindings === null) delete next[id];
  else next[id] = bindings;
  current = next;
  writeJson(SHORTCUTS_STORAGE_KEY, Object.keys(next).length ? next : null);
  emit();
}

export function resetAllShortcuts(): void {
  current = {};
  writeJson(SHORTCUTS_STORAGE_KEY, null);
  emit();
}
