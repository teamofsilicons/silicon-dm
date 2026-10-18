"use client";

/**
 * The ⌘K command palette's registry. The shell registers navigation, theme and account commands; any page can add its
 * own while it is mounted:
 *
 *   useRegisterCommands(() => [{ id: "items.create", label: "New item", group: "Items", run: openCreate }], [openCreate]);
 *
 * Commands registered from a component are removed when it unmounts. Page commands list before the shell's.
 */
import { useEffect, useSyncExternalStore, type DependencyList, type ReactNode } from "react";

export interface ShellCommand {
  /** Unique across the page (later registrations win for the same id). */
  id: string;
  label: string;
  description?: string;
  /** Group heading in the palette ("Go to", "Appearance", "Items"…). */
  group?: string;
  keywords?: string[];
  /** Other names it answers to as itself (a conversation's address; the other account's id in a direct one). */
  names?: string[];
  /** The people in a conversation, by id and name (lib/command-rank.ts ranks them below its own names). */
  members?: string[];
  /** A conversation to open: it leads a tie, and the empty palette lists the most recent few (lib/command-rank.ts). */
  conversation?: boolean;
  icon?: ReactNode;
  /** A hint shown at the end of the row, for example "4". */
  shortcut?: string;
  /** The keyboard shortcut (lib/shortcuts/registry.ts) that does the same: its current keys show at the end of the row. */
  shortcutId?: string;
  run: () => void;
}

interface Source {
  id: number;
  commands: ShellCommand[];
}

let sources: Source[] = [];
let open = false;
/** What is typed in the palette while it is open ("" otherwise): the chat looks through older conversations for it. */
let query = "";
let nextId = 0;
let snapshot: ShellCommand[] = [];
const listeners = new Set<() => void>();

function rebuild() {
  const seen = new Set<string>();
  const out: ShellCommand[] = [];
  for (const source of [...sources].reverse()) {
    for (const command of source.commands) {
      if (seen.has(command.id)) continue;
      seen.add(command.id);
      out.push(command);
    }
  }
  snapshot = out;
}

function emit() {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Adds commands until the returned function is called. */
export function registerCommands(commands: ShellCommand[]): () => void {
  const id = ++nextId;
  sources = [...sources, { id, commands }];
  rebuild();
  emit();
  return () => {
    sources = sources.filter(source => source.id !== id);
    rebuild();
    emit();
  };
}

/** Registers commands for as long as the component is mounted; `build` reruns when `deps` change. */
export function useRegisterCommands(build: () => ShellCommand[], deps: DependencyList): void {
  // eslint-disable-next-line react-hooks/exhaustive-deps -- the caller lists what `build` reads, like useMemo.
  useEffect(() => registerCommands(build()), deps);
}

const EMPTY: ShellCommand[] = [];

/** Every registered command, later registrations first so page commands lead. */
export function useCommands(): ShellCommand[] {
  return useSyncExternalStore(subscribe, () => snapshot, () => EMPTY);
}

export function useCommandPaletteOpen(): boolean {
  return useSyncExternalStore(subscribe, () => open, () => false);
}

export function setCommandPaletteOpen(next: boolean): void {
  if (open === next) return;
  open = next;
  if (!next) query = "";
  emit();
}

/** The palette says what is typed in it (components/foundation/shell/command-menu.tsx). */
export function setCommandPaletteQuery(next: string): void {
  const value = open ? next : "";
  if (value === query) return;
  query = value;
  emit();
}

/** What is typed in the open palette ("" when it is closed or empty). */
export function useCommandPaletteQuery(): string {
  return useSyncExternalStore(subscribe, () => query, () => "");
}

export const openCommandPalette = () => setCommandPaletteOpen(true);
export const closeCommandPalette = () => setCommandPaletteOpen(false);
export const toggleCommandPalette = () => setCommandPaletteOpen(!open);
