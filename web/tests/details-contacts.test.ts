/** Who the account picker offers (components/chat/dialogs/contacts.ts): people you talk to, then your Silicons. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { isListed, keySet, kindOfId, knownAccounts, matchAccounts, typedAccount } from "../components/chat/dialogs/contacts";
import type { AccountRef, ConversationSummary } from "../lib/chat/types";

const ada: AccountRef = { type: "carbon", id: "c:ada", uuid: "u-ada", display_name: "Ada Marlow" };
const bo: AccountRef = { type: "carbon", id: "c:bo", uuid: "u-bo", display_name: "Bo Okafor" };
const cy: AccountRef = { type: "carbon", id: "c:cy", uuid: "u-cy", display_name: "Cy Varga" };
const scout: AccountRef = { type: "silicon", id: "si:scout", uuid: "u-scout", display_name: "Scout" };
const quill: AccountRef = { type: "silicon", id: "si:quill", uuid: "u-quill" };
const gone: AccountRef = { type: "carbon", id: "u-gone", uuid: "u-gone", deleted: true };

const summary = (id: string, participants: AccountRef[]): ConversationSummary => ({ id, participants, last_message: null, created_at: "2026-10-17T00:00:00Z", updated_at: "2026-10-17T00:00:00Z" });

test("people you talk to come first, newest conversation first, then your Silicons; you, duplicates and deleted accounts never", () => {
  const conversations = [summary("c:ada::c:cy", [ada, cy]), summary("g:team-notes", [ada, bo, cy, gone]), summary("c:ada::si:scout", [ada, scout])];
  const known = knownAccounts(conversations, { account: ada, custodian_of: [scout, quill] });
  assert.deepEqual(known.map(account => account.id), ["c:cy", "c:bo", "si:scout", "si:quill"]);
});

test("matching: an id that starts with the text first, then a name, then anything containing it", () => {
  const known = [cy, bo, scout, quill];
  assert.deepEqual(matchAccounts(known, "si:").map(account => account.id), ["si:scout", "si:quill"]);
  assert.deepEqual(matchAccounts(known, "bo").map(account => account.id), ["c:bo"]);
  assert.deepEqual(matchAccounts(known, "okafor").map(account => account.id), ["c:bo"]);
  assert.deepEqual(matchAccounts(known, "qu").map(account => account.id), ["si:quill"]);
  assert.deepEqual(matchAccounts(known, "ou").map(account => account.id), ["si:scout"]);
  assert.deepEqual(matchAccounts(known, "").map(account => account.id), ["c:cy", "c:bo", "si:scout", "si:quill"]);
  assert.deepEqual(matchAccounts(known, "", 2).map(account => account.id), ["c:cy", "c:bo"]);
  assert.deepEqual(matchAccounts(known, "zzz"), []);
});

test("typed ids are checked and normalised, with the reason when they are not ids", () => {
  assert.deepEqual(typedAccount("  C:Grace "), { ok: true, account: { type: "carbon", id: "c:grace" } });
  assert.deepEqual(typedAccount("si:atlas"), { ok: true, account: { type: "silicon", id: "si:atlas" } });
  const email = typedAccount("ada@example.com");
  assert.equal(email.ok, false);
  assert.match(email.ok ? "" : email.message, /email address/);
  const bare = typedAccount("grace");
  assert.match(bare.ok ? "" : bare.message, /no c: or si: in front/);
  assert.equal(kindOfId("si:x"), "silicon");
  assert.equal(kindOfId("c:x"), "carbon");
});

test("listed accounts are found by uuid or by id, ids without regard to case", () => {
  const keys = keySet([bo, "C:Cy"]);
  assert.equal(isListed({ id: "c:bo-renamed", uuid: "u-bo" }, keys), true);
  assert.equal(isListed({ id: "c:cy" }, keys), true);
  assert.equal(isListed(scout, keys), false);
});

test("a refusal's words name the chosen ids it is about", async () => {
  const { idsNamedIn } = await import("../components/chat/dialogs/contacts");
  const chosen = [{ id: "si:scout" }, { id: "si:scout-2" }, { id: "c:bo" }];
  assert.deepEqual(idsNamedIn("si:scout belongs to a deleted Silicon Accounts account; it cannot receive messages or invitations.", chosen), ["si:scout"]);
  assert.deepEqual(idsNamedIn("No Silicon Accounts account has the id c:bo now.", chosen), ["c:bo"]);
  assert.deepEqual(idsNamedIn("Something else went wrong.", chosen), []);
});
