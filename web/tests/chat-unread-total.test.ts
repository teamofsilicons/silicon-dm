/**
 * The title's "(n)" counts every conversation (lib/chat/unread-total.ts): the service's total for the conversations not
 * loaded, plus the loaded rows as they are now; a later page takes its rows out of the part not loaded.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { totalUnread } from "../lib/chat/cache";
import type { AccountRef, ConversationSummary } from "../lib/chat/types";
import { noteListHead, noteListPage, resetUnreadTotal, unreadNotLoaded } from "../lib/chat/unread-total";

const ada: AccountRef = { type: "carbon", id: "c:ada", uuid: "u-ada" };
const row = (id: string, unread: number): ConversationSummary => ({ id, participants: [ada], last_message: null, created_at: "2026-10-01T00:00:00Z", updated_at: "2026-10-01T00:00:00Z", unread_count: unread });
const title = (loaded: ConversationSummary[]) => (unreadNotLoaded() ?? 0) + totalUnread(loaded, ada);

test("the title counts the conversations not loaded (the service's total) and the loaded rows as they change", () => {
  resetUnreadTotal();
  let loaded = [row("a", 2), row("b", 0)];
  assert.equal(title(loaded), 2, "an older service: the loaded rows");
  noteListHead({ items: loaded, next_cursor: "p1", unread: { conversations: 3, messages: 10 } }, []);
  assert.equal(unreadNotLoaded(), 8, "8 unread further down the list");
  assert.equal(title(loaded), 10);
  // A message arrives in a, then b is read on another device: the rows change, the part not loaded does not.
  loaded = [row("a", 3), row("b", 0)];
  assert.equal(title(loaded), 11);
  // The next page loads: c (5 unread) was in the total all along.
  const next = [row("c", 5), row("d", 0)];
  noteListPage({ items: next, next_cursor: null });
  assert.equal(unreadNotLoaded(), 3);
  loaded = [...loaded, ...next];
  assert.equal(title(loaded), 11, "nothing counted twice");
  noteListPage({ items: next, next_cursor: null });
  assert.equal(unreadNotLoaded(), 3, "a page loaded again takes nothing more");
  // A fresh first page starts over, counting what this tab has loaded besides it.
  noteListHead({ items: [row("a", 0), row("b", 0)], next_cursor: "p1", unread: { conversations: 1, messages: 5 } }, loaded);
  assert.equal(unreadNotLoaded(), 0, "c's 5 are loaded");
  assert.equal(title([row("a", 0), row("b", 0), row("c", 5), row("d", 0)]), 5);
  // A first page without a total (an older service) goes back to the loaded rows.
  noteListHead({ items: [row("a", 1)], next_cursor: null }, []);
  assert.equal(unreadNotLoaded(), null);
  resetUnreadTotal();
});

test("never below nothing: the service counts at most 1,000, and the rows' counts may add up to more", () => {
  resetUnreadTotal();
  noteListHead({ items: [row("a", 1000), row("b", 800)], next_cursor: null, unread: { conversations: 2, messages: 1000 } }, []);
  assert.equal(unreadNotLoaded(), 0);
  resetUnreadTotal();
});
