"use client";

/**
 * Drafts at run time (the merge rules are lib/chat/drafts.ts). What you type is in localStorage at once (per account
 * and conversation, so it is back the moment you return) and saved to the service about 1.5 s after you stop, with
 * the version you last saw. A 409 means another device saved first: the text here wins and is saved again against
 * that newer version. Returning to the tab looks for a newer draft from another device. Sending clears the draft
 * here and on the service.
 */
import { useCallback, useEffect, useSyncExternalStore } from "react";
import { ApiError } from "@/lib/errors";
import { deleteDraft, getDraft, putDraft } from "./api";
import { editDraft, hasContent, mergeDraft, savedDraft, type LocalDraft, type ServerDraft } from "./drafts";
import { readJson, writeJson } from "./storage";
import type { Draft } from "./types";

const SAVE_AFTER_MS = 1500;
const RETRY_AFTER_MS = 10_000;
const keyFor = (account: string, address: string) => `dm:draft:v1:${account}:${address}`;

export type DraftSaveState = "idle" | "saving" | "saved" | "offline";

interface Entry {
  draft: LocalDraft | null;
  save: DraftSaveState;
  timer: ReturnType<typeof setTimeout> | null;
  saving: Promise<void> | null;
  loaded: boolean;
  listeners: Set<() => void>;
  snapshot: { draft: LocalDraft | null; save: DraftSaveState };
}

const entries = new Map<string, Entry>();
/** Everything that shows drafts of any conversation (the rows' previews). */
const anyListeners = new Set<() => void>();

function entryFor(account: string, address: string): Entry {
  const key = keyFor(account, address);
  let entry = entries.get(key);
  if (!entry) {
    const draft = readJson<LocalDraft | null>(key, null);
    entry = { draft, save: "idle", timer: null, saving: null, loaded: false, listeners: new Set(), snapshot: { draft, save: "idle" } };
    entries.set(key, entry);
  }
  return entry;
}

function commit(account: string, address: string, entry: Entry, draft: LocalDraft | null, save: DraftSaveState = entry.save) {
  entry.draft = draft && (hasContent(draft) || draft.version > 0 || draft.dirty) ? draft : null;
  entry.save = save;
  entry.snapshot = { draft: entry.draft, save };
  writeJson(keyFor(account, address), entry.draft && hasContent(entry.draft) ? entry.draft : entry.draft && entry.draft.version > 0 ? entry.draft : null);
  for (const listener of entry.listeners) listener();
  for (const listener of anyListeners) listener();
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
      commit(account, address, entry, draft, "saving");
      try {
        if (!hasContent(draft)) {
          if (draft.version > 0) await deleteDraft(address);
          commit(account, address, entry, entry.draft ? savedDraft({ ...entry.draft, version: 0 }, sent, 0) : null, "saved");
          return;
        }
        const result = await putDraft(address, { message_content: draft.text, reply_to_message_id: draft.replyTo }, draft.version);
        commit(account, address, entry, entry.draft ? savedDraft(entry.draft, sent, result.version) : null, "saved");
        return;
      } catch (error) {
        const failure = ApiError.from(error);
        if (failure.status === 409) {
          // Another device saved first (or removed it): save this text again against what the service has now.
          const server = await fetchServer(address).catch(() => null);
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
  entry.saving = run().finally(() => {
    entry.saving = null;
    // Typing during the save left more to save.
    if (entry.draft?.dirty && !entry.timer) schedule(account, address, entry);
  });
  return entry.saving;
}

function schedule(account: string, address: string, entry: Entry) {
  if (entry.timer) clearTimeout(entry.timer);
  entry.timer = setTimeout(() => {
    entry.timer = null;
    void save(account, address, entry);
  }, SAVE_AFTER_MS);
}

/** Looks at the service's draft and merges it with the local one. */
async function refresh(account: string, address: string, entry: Entry): Promise<void> {
  let server: ServerDraft | null;
  try {
    server = await fetchServer(address);
  } catch {
    return;
  }
  // Typing may have happened while the answer was on its way: merge with the draft as it is now.
  const result = mergeDraft(entry.draft, server, Date.now());
  entry.loaded = true;
  if (result.action === "none" && result.draft === entry.draft) return;
  commit(account, address, entry, result.draft);
  if (result.action === "push") schedule(account, address, entry);
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

/* What the hook's callbacks do, kept outside the component: the entries are shared, mutable module state. */

function changeDraft(account: string, address: string, change: Partial<Pick<LocalDraft, "text" | "replyTo">>) {
  const entry = entryFor(account, address);
  commit(account, address, entry, editDraft(entry.draft, change, Date.now()), entry.save);
  schedule(account, address, entry);
}

function clearDraft(account: string, address: string, remote: boolean) {
  const entry = entryFor(account, address);
  if (entry.timer) clearTimeout(entry.timer);
  entry.timer = null;
  const hadServerDraft = (entry.draft?.version ?? 0) > 0;
  commit(account, address, entry, null, "idle");
  // The service also clears a draft whose content was just sent; deleting is idempotent either way.
  if (remote && hadServerDraft) void deleteDraft(address).catch(() => undefined);
}

/** Starts following one conversation's draft (and the service's), until the returned function is called. */
function followDraft(account: string, address: string): () => void {
  const entry = entryFor(account, address);
  void refresh(account, address, entry);
  const onFocus = () => {
    if (!entry.draft?.dirty) void refresh(account, address, entry);
  };
  window.addEventListener("focus", onFocus);
  return () => {
    window.removeEventListener("focus", onFocus);
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
 * a Silicon's draft but never write it), where nothing is stored or saved.
 */
export function useDraft(account: string, address: string, enabled = true): DraftHandle {
  const snapshot = useSyncExternalStore(
    useCallback((listener: () => void) => subscribeEntry(account, address, listener), [account, address]),
    () => snapshotOf(account, address),
    () => EMPTY_SNAPSHOT,
  );
  useEffect(() => (enabled ? followDraft(account, address) : undefined), [account, address, enabled]);
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
