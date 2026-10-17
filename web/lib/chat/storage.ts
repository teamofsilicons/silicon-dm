/**
 * Browser storage that never breaks the page: private windows, blocked site data and full quotas make storage throw,
 * and the website must work without it (it only loses what it remembers between visits). Every key DM writes starts
 * with "dm:" and carries a version, so a later format can move on without misreading an older one.
 */

type Area = "local" | "session";

function area(kind: Area): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return kind === "local" ? window.localStorage : window.sessionStorage;
  } catch {
    return null;
  }
}

const usable: Partial<Record<Area, boolean>> = {};

/** Whether this browser lets the page keep anything (a private window or blocked site data may not). */
export function canStore(kind: Area = "local"): boolean {
  if (usable[kind] !== undefined) return usable[kind]!;
  try {
    const store = area(kind);
    if (!store) return false;
    store.setItem("dm:probe", "1");
    store.removeItem("dm:probe");
    usable[kind] = true;
  } catch {
    usable[kind] = false;
  }
  return usable[kind]!;
}

export function readJson<T>(key: string, fallback: T, kind: Area = "local"): T {
  try {
    const raw = area(kind)?.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

/** Writes JSON (removes the key for null/undefined). Returns false when storage refused it. */
export function writeJson(key: string, value: unknown, kind: Area = "local"): boolean {
  try {
    const store = area(kind);
    if (!store) return false;
    if (value === null || value === undefined) store.removeItem(key);
    else store.setItem(key, JSON.stringify(value));
    return true;
  } catch {
    return false;
  }
}

export function readText(key: string, kind: Area = "local"): string | null {
  try {
    return area(kind)?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

export function writeText(key: string, value: string | null, kind: Area = "local"): void {
  try {
    const store = area(kind);
    if (!store) return;
    if (value === null) store.removeItem(key);
    else store.setItem(key, value);
  } catch {
    // Storage refused (private window, quota): the value lives for this page only.
  }
}

/** Calls `listener` when another tab changes `key` (the storage event never fires in the tab that wrote it). */
export function onStorage(key: string, listener: () => void): () => void {
  if (typeof window === "undefined") return () => undefined;
  const handler = (event: StorageEvent) => {
    if (event.key === key || event.key === null) listener();
  };
  window.addEventListener("storage", handler);
  return () => window.removeEventListener("storage", handler);
}
