"use client";

/**
 * Drafts at run time (the merge rules are lib/chat/drafts.ts). What you type is in localStorage at once (per account
 * and conversation, so it is back the moment you return) and saved to the service about 1.5 s after you stop, with
 * the version you last saw. A 409 means another device saved first: the text here wins and is saved again against
 * that newer version. Opening a conversation, and returning to the tab, look for a newer draft from another device.
 * Sending clears the draft here and on the service.
 *
 * Looking costs one request at most, shared by everything that asks at once (React's double effects in development,
 * the tab's focus), and none when the conversation list already says what the service has (`draft_version`, see
 * drafts.ts draftCheck): a draft that is not there answers 404, which the browser logs as an error. That answer is
 * "no draft", never a failure. Returning to the tab looks again at most every 30 seconds.
 *
 * Tabs of one browser share the stored drafts: each follows the others' writes (the `storage` event, and a fresh read
 * when a composer mounts or the tab comes back), so a message sent in one tab leaves the others' fields too, and a tab
 * that was open all along never answers a long draft typed elsewhere with one stale keystroke (drafts.ts adoptStored).
 * A message sent while its draft's save is on its way would leave that save as the service's draft (the words came
 * back on every device): its draft is deleted once that save has answered.
 */
import { useCallback, useEffect, useSyncExternalStore } from "react";
import { ApiError } from "@/lib/errors";
import { deleteDraft, getDraft, putDraft } from "./api";
import { adoptStored, draftCheck, editDraft, hasContent, mergeDraft, savedDraft, type DraftHint, type LocalDraft, type ServerDraft } from "./drafts";
import { readJson, writeJson } from "./storage";
import type { Draft } from "./types";

const SAVE_AFTER_MS = 1500;
const RETRY_AFTER_MS = 10_000;
/** Returning to the tab looks for another device's draft at most this often. */
const LOOK_AGAIN_AFTER_MS = 30_000;
const keyFor = (account: string, address: string) => `dm:draft:v1:${account}:${address}`;

export type DraftSaveState = "idle" | "saving" | "saved" | "offline";

interface Entry {
  draft: LocalDraft | null;
  save: DraftSaveState;
  timer: ReturnType<typeof setTimeout> | null;
  saving: Promise<void> | null;
  loaded: boolean;
  /** A look at the service's draft under way: everything that asks meanwhile shares it. */
  looking: Promise<void> | null;
  /** When the service last answered a look (epoch ms; 0: never). */
  lookedAt: number;
  /** What the conversation list says about the service's draft (drafts.ts DraftHint). */
  hint: DraftHint;
  /** Counts the times the draft was sent or discarded: a save's late answer for an older one leaves the new one alone. */
  cleared: number;
  listeners: Set<() => void>;
  snapshot: { draft: LocalDraft | null; save: DraftSaveState };
}

const entries = new Map<string, Entry>();
/** Everything that shows drafts of any conversation (the rows' previews). */
const anyListeners = new Set<() => void>();

const KEY_PREFIX = "dm:draft:v1:";

function entryFor(account: string, address: string): Entry {
  const key = keyFor(account, address);
  let entry = entries.get(key);
  if (!entry) {
    const draft = readJson<LocalDraft | null>(key, null);
    entry = { draft, save: "idle", timer: null, saving: null, loaded: false, looking: null, lookedAt: 0, hint: "unknown", cleared: 0, listeners: new Set(), snapshot: { draft, save: "idle" } };
    entries.set(key, entry);
    followOtherTabs();
  }
  return entry;
}

function notify(entry: Entry) {
  for (const listener of entry.listeners) listener();
  for (const listener of anyListeners) listener();
}

/** What a draft looks like in storage: one with words, or an emptied one the service still has (to be deleted). */
const storedForm = (draft: LocalDraft | null) => (draft && hasContent(draft) ? draft : draft && draft.version > 0 ? draft : null);

function commit(account: string, address: string, entry: Entry, draft: LocalDraft | null, save: DraftSaveState = entry.save) {
  entry.draft = draft && (hasContent(draft) || draft.version > 0 || draft.dirty) ? draft : null;
  entry.save = save;
  entry.snapshot = { draft: entry.draft, save };
  writeJson(keyFor(account, address), storedForm(entry.draft));
  notify(entry);
}

/** Takes another tab's word for a draft (drafts.ts adoptStored), in memory only: storage already says it. */
function adopt(entry: Entry, stored: LocalDraft | null) {
  const next = adoptStored(entry.draft, stored);
  if (next === entry.draft) return;
  entry.draft = next;
  entry.snapshot = { draft: next, save: entry.save };
  notify(entry);
}

/** Reads one conversation's stored draft again (a composer mounting, the tab coming back: a write may have been missed). */
function readStored(account: string, address: string, entry: Entry) {
  adopt(entry, readJson<LocalDraft | null>(keyFor(account, address), null));
}

let following = false;

/** Follows the drafts other tabs of this browser write (the storage event never fires in the tab that wrote). */
function followOtherTabs() {
  if (following || typeof window === "undefined") return;
  following = true;
  window.addEventListener("storage", event => {
    if (event.key === null) {
      // Storage was cleared (sign-out elsewhere, site data removed): every draft here is gone too, unless unsaved.
      for (const entry of entries.values()) adopt(entry, null);
      return;
    }
    if (!event.key.startsWith(KEY_PREFIX)) return;
    const entry = entries.get(event.key);
    if (!entry) {
      // Not open here: the rows' previews read storage.
      for (const listener of anyListeners) listener();
      return;
    }
    let stored: LocalDraft | null = null;
    try {
      stored = event.newValue ? (JSON.parse(event.newValue) as LocalDraft) : null;
    } catch {
      stored = null;
    }
    adopt(entry, stored);
  });
}

const fromServer = (draft: Draft): ServerDraft => ({ text: draft.message_content ?? "", replyTo: draft.reply_to_message_id ?? null, version: draft.version, updatedAt: Date.parse(draft.updated_at) || 0 });

async function fetchServer(address: string): Promise<ServerDraft | null> {
  try {
    return fromServer(await getDraft(address));
  } catch (error) {
    if (ApiError.from(error).status === 404) return null;
    throw error;
  }
}

/** Saves the current text (or removes the service's draft when the text is gone). Retries once after a 409. */
async function save(account: string, address: string, entry: Entry): Promise<void> {
  if (entry.saving) return entry.saving;
  const run = async () => {
    for (let attempt = 0; attempt < 2; attempt++) {
      const draft = entry.draft;
      if (!draft || !draft.dirty) return;
      const sent = { text: draft.text, replyTo: draft.replyTo };
      // Sent or discarded while this was on its way: the answer is about that draft, not what is typed now (clearDraft
      // deletes the service's copy once this has answered).
      const generation = entry.cleared;
      const stale = () => entry.cleared !== generation;
      commit(account, address, entry, draft, "saving");
      try {
        if (!hasContent(draft)) {
          if (draft.version > 0) await deleteDraft(address);
          if (stale()) return;
          commit(account, address, entry, entry.draft ? savedDraft({ ...entry.draft, version: 0 }, sent, 0) : null, "saved");
          return;
        }
        const result = await putDraft(address, { message_content: draft.text, reply_to_message_id: draft.replyTo }, draft.version);
        if (stale()) return;
        commit(account, address, entry, entry.draft ? savedDraft(entry.draft, sent, result.version) : null, "saved");
        return;
      } catch (error) {
        if (stale()) return;
        const failure = ApiError.from(error);
        if (failure.status === 409) {
          // Another device saved first (or removed it): save this text again against what the service has now.
          const server = await fetchServer(address).catch(() => null);
          if (stale()) return;
          if (entry.draft) commit(account, address, entry, { ...entry.draft, version: server?.version ?? 0, dirty: true }, "saving");
          continue;
        }
        commit(account, address, entry, entry.draft, failure.isNetwork ? "offline" : "idle");
        if (entry.timer) clearTimeout(entry.timer);
        entry.timer = setTimeout(() => void save(account, address, entry), RETRY_AFTER_MS);
        return;
      }
    }
  };
  const saving: Promise<void> = run().finally(() => {
    // A send meanwhile may have put a delete behind this save (clearDraft): that one is the save under way now.
    if (entry.saving === saving) entry.saving = null;
    // Typing during the save left more to save.
    if (entry.draft?.dirty && !entry.timer && !entry.saving) schedule(account, address, entry);
  });
  entry.saving = saving;
  return saving;
}

function schedule(account: string, address: string, entry: Entry) {
  if (entry.timer) clearTimeout(entry.timer);
  entry.timer = setTimeout(() => {
    entry.timer = null;
    void save(account, address, entry);
  }, SAVE_AFTER_MS);
}

/** Merges what the service has (null: no draft) with the local draft. */
function settle(account: string, address: string, entry: Entry, server: ServerDraft | null) {
  const result = mergeDraft(entry.draft, server, Date.now());
  entry.loaded = true;
  if (result.action === "none" && result.draft === entry.draft) return;
  commit(account, address, entry, result.draft);
  if (result.action === "push") schedule(account, address, entry);
}

/**
 * Looks at the service's draft and merges it with the local one: not at all when the list's word settles it, and once
 * for everyone who asks while a look is under way.
 */
function refresh(account: string, address: string, entry: Entry): Promise<void> {
  const check = draftCheck(entry.draft, entry.hint);
  if (check === "wait" || check === "same") return Promise.resolve();
  if (check === "absent") {
    settle(account, address, entry, null);
    return Promise.resolve();
  }
  entry.looking ??= (async () => {
    let server: ServerDraft | null;
    try {
      server = await fetchServer(address);
    } catch {
      return;
    }
    entry.lookedAt = Date.now();
    // Typing may have happened while the answer was on its way: merge with the draft as it is now.
    settle(account, address, entry, server);
  })().finally(() => {
    entry.looking = null;
  });
  return entry.looking;
}

export interface DraftHandle {
  text: string;
  replyTo: string | null;
  save: DraftSaveState;
  setText: (text: string) => void;
  setReplyTo: (code: string | null) => void;
  /** The message was sent (or the Carbon discarded the draft): gone here and on the service. */
  clear: () => void;
}

/* What the hook's callbacks do, kept outside the component: the entries are shared, mutable module state. The composer
   goes through useDraft; these two are exported for tests/draft-tabs.test.ts. */

export function changeDraft(account: string, address: string, change: Partial<Pick<LocalDraft, "text" | "replyTo">>) {
  const entry = entryFor(account, address);
  commit(account, address, entry, editDraft(entry.draft, change, Date.now()), entry.save);
  schedule(account, address, entry);
}

export function clearDraft(account: string, address: string, remote: boolean) {
  const entry = entryFor(account, address);
  if (entry.timer) clearTimeout(entry.timer);
  entry.timer = null;
  const hadServerDraft = (entry.draft?.version ?? 0) > 0;
  const onItsWay = entry.saving;
  entry.cleared += 1;
  commit(account, address, entry, null, "idle");
  if (!remote) return;
  if (onItsWay) {
    // A save on its way lands after this: the service would keep the words just sent as its draft (its clear on send
    // matches only the words saved before the send, and the message is sent trimmed). Delete it once that save has
    // answered; a new draft typed meanwhile is saved after this, never before it.
    const deleting: Promise<void> = onItsWay
      .catch(() => undefined)
      .then(() => deleteDraft(address))
      .then(() => undefined, () => undefined)
      .finally(() => {
        if (entry.saving === deleting) entry.saving = null;
        if (entry.draft?.dirty && !entry.timer && !entry.saving) schedule(account, address, entry);
      });
    entry.saving = deleting;
    return;
  }
  // The service also clears a draft whose content was just sent; deleting is idempotent either way.
  if (hadServerDraft) void deleteDraft(address).catch(() => undefined);
}

/**
 * Takes the conversation list's word about the service's draft, and looks when that is not enough. Opening the same
 * conversation again within 30 seconds of a look, with nothing new said, does not look again.
 */
export function lookForDraft(account: string, address: string, hint: DraftHint): Promise<void> {
  const entry = entryFor(account, address);
  const changed = entry.hint !== hint;
  entry.hint = hint;
  if (!changed && entry.lookedAt && Date.now() - entry.lookedAt < LOOK_AGAIN_AFTER_MS) return Promise.resolve();
  return refresh(account, address, entry);
}

/** Starts following one conversation's draft (and the service's), until the returned function is called. */
function followDraft(account: string, address: string): () => void {
  const entry = entryFor(account, address);
  // Another tab may have written while this one was away (a page kept in the back-forward cache misses the events).
  readStored(account, address, entry);
  const onFocus = () => {
    if (document.visibilityState === "hidden") return;
    readStored(account, address, entry);
    if (!entry.draft?.dirty && Date.now() - entry.lookedAt >= LOOK_AGAIN_AFTER_MS) void refresh(account, address, entry);
  };
  window.addEventListener("focus", onFocus);
  document.addEventListener("visibilitychange", onFocus);
  return () => {
    window.removeEventListener("focus", onFocus);
    document.removeEventListener("visibilitychange", onFocus);
    // Leaving the conversation saves what was typed right away.
    if (entry.timer) {
      clearTimeout(entry.timer);
      entry.timer = null;
      void save(account, address, entry);
    }
  };
}

function subscribeEntry(account: string, address: string, listener: () => void): () => void {
  const entry = entryFor(account, address);
  entry.listeners.add(listener);
  return () => entry.listeners.delete(listener);
}

const snapshotOf = (account: string, address: string) => entryFor(account, address).snapshot;

/**
 * The draft of one conversation for the composer. `enabled` is false in a custodian's read-only view (custodians read
 * a Silicon's draft but never write it), where nothing is stored or saved. `hint` is what the conversation list says
 * about the service's draft (drafts.ts draftHint); with "unknown" the service is asked once per opening.
 */
export function useDraft(account: string, address: string, enabled = true, hint: DraftHint = "unknown"): DraftHandle {
  const snapshot = useSyncExternalStore(
    useCallback((listener: () => void) => subscribeEntry(account, address, listener), [account, address]),
    () => snapshotOf(account, address),
    () => EMPTY_SNAPSHOT,
  );
  useEffect(() => (enabled ? followDraft(account, address) : undefined), [account, address, enabled]);
  useEffect(() => {
    if (enabled) void lookForDraft(account, address, hint);
  }, [account, address, enabled, hint]);
  const setText = useCallback((text: string) => {
    if (enabled) changeDraft(account, address, { text });
  }, [account, address, enabled]);
  const setReplyTo = useCallback((replyTo: string | null) => {
    if (enabled) changeDraft(account, address, { replyTo });
  }, [account, address, enabled]);
  const clear = useCallback(() => clearDraft(account, address, enabled), [account, address, enabled]);
  return { text: snapshot.draft?.text ?? "", replyTo: snapshot.draft?.replyTo ?? null, save: snapshot.save, setText, setReplyTo, clear };
}

const EMPTY_SNAPSHOT: { draft: LocalDraft | null; save: DraftSaveState } = { draft: null, save: "idle" };

/** A conversation's draft as this tab holds it (what the composer shows). */
export function draftOf(account: string, address: string): { text: string; replyTo: string | null; version: number } | null {
  const draft = entryFor(account, address).draft;
  return draft ? { text: draft.text, replyTo: draft.replyTo, version: draft.version } : null;
}

/** The local draft text of a conversation, for a row's "Draft:" preview (no network). */
export function localDraftText(account: string, address: string): string {
  const entry = entries.get(keyFor(account, address));
  const draft = entry ? entry.draft : readJson<LocalDraft | null>(keyFor(account, address), null);
  return draft && hasContent(draft) ? draft.text : "";
}

/** Calls `listener` when any draft changes in this tab (rows' previews). */
export function subscribeDrafts(listener: () => void): () => void {
  anyListeners.add(listener);
  return () => anyListeners.delete(listener);
}
