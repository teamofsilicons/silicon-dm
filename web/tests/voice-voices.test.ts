/** Voices (lib/voice/voices.ts): the website marks the same default the service gives, and samples say the right line. */
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { defaultVoice, defaultVoiceIndex, sampleName, sampleScript } from "../lib/voice/voices";

const catalog = ["mark", "brittney", "jamal", "jessa", "hope"].map(key => ({ key, name: key, description: "" }));

/** The service's rule, written the way src/domain/speech.rs states it. */
const oracle = (uuid: string, count: number) => Number(createHash("sha256").update(uuid).digest().readBigUInt64BE(0) % BigInt(count));

test("the default voice follows SHA-256 of the account's uuid, like the service", async () => {
  assert.equal(await defaultVoiceIndex("a8K", 5), 3, "the service's own test vector");
  for (const uuid of ["5d0b7f5e-9c55-4e1b-9d7a-2f1c8b9a0e11", "uuid-1", "x", "0193f3b2-7c41-7a3e-8f00-000000000000"]) {
    assert.equal(await defaultVoiceIndex(uuid, 5), oracle(uuid, 5), uuid);
    assert.equal(await defaultVoiceIndex(uuid, 3), oracle(uuid, 3), uuid);
  }
  assert.equal(await defaultVoiceIndex("x", 0), 0);
  assert.equal((await defaultVoice({ uuid: "a8K" }, catalog))?.key, "jessa");
  assert.equal(await defaultVoice({}, catalog), null);
  const spread = new Set(await Promise.all(Array.from({ length: 40 }, (_, n) => defaultVoiceIndex(`uuid-${n}`, 5))));
  assert.ok(spread.size >= 4, "defaults spread over the catalog");
});

test("a sample introduces the account by its first name", () => {
  assert.equal(sampleName({ id: "si:scout", display_name: "Scout" }), "Scout");
  assert.equal(sampleName({ id: "c:ada", display_name: "Ada Lovelace" }), "Ada");
  assert.equal(sampleName({ id: "si:quill" }), "Quill");
  assert.equal(sampleScript("Scout"), "Hi, I'm Scout. This is how I'll sound when you listen to my messages.");
});

test("a sample's name is a word, and never a cue the service would act or refuse", () => {
  assert.equal(sampleName({ id: "si:scout", display_name: "🤖 Scout Bot" }), "Scout", "the first word with a letter");
  assert.equal(sampleName({ id: "si:scout", display_name: "  " }), "Scout", "blank names fall back to the handle");
  assert.equal(sampleScript("[Bot]"), "Hi, I'm Bot. This is how I'll sound when you listen to my messages.");
  assert.equal(sampleScript("Scout]"), "Hi, I'm Scout. This is how I'll sound when you listen to my messages.", "an unbalanced bracket would be refused");
  assert.equal(sampleScript("[]"), "Hi. This is how I'll sound when you listen to my messages.");
});
