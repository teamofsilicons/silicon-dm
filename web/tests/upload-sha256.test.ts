/**
 * The browser's SHA-256 (lib/upload/sha256.ts), which Silicon Briefcase checks every upload's bytes against: the FIPS
 * 180-2 examples, the million-"a" vector, every length around the block and padding edges, and node:crypto over random
 * inputs cut into random pieces (a file is fed slice by slice, and a slice can end anywhere).
 */
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { test } from "node:test";
import { Sha256, sha256Hex, toHex } from "../lib/upload/sha256";

const encode = (text: string) => new TextEncoder().encode(text);
const reference = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");

test("the FIPS 180-2 examples", () => {
  assert.equal(sha256Hex(encode("")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
  assert.equal(sha256Hex(encode("abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  assert.equal(sha256Hex(encode("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
  assert.equal(
    sha256Hex(encode("abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
    "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
  );
});

test("one million times \"a\", fed in pieces of 1000", () => {
  const hash = new Sha256();
  const piece = encode("a".repeat(1000));
  for (let i = 0; i < 1000; i++) hash.update(piece);
  assert.equal(hash.hex(), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
});

test("every length across the block and padding edges (0 to 300 bytes) matches node:crypto", () => {
  const bytes = randomBytes(300);
  for (let length = 0; length <= 300; length++) assert.equal(sha256Hex(bytes.subarray(0, length)), reference(bytes.subarray(0, length)), `length ${length}`);
});

test("random inputs cut into random pieces match node:crypto, whatever the cuts", () => {
  for (let round = 0; round < 40; round++) {
    const length = Math.floor(Math.random() * 300_000);
    const bytes = randomBytes(length);
    const hash = new Sha256();
    for (let at = 0; at < length; ) {
      const step = Math.min(length - at, 1 + Math.floor(Math.random() * (Math.random() < 0.5 ? 70 : 70_000)));
      hash.update(bytes.subarray(at, at + step));
      at += step;
    }
    assert.equal(hash.hex(), reference(bytes), `round ${round}, ${length} bytes`);
  }
});

test("a view into a larger buffer is read from its own offset", () => {
  const backing = randomBytes(10_000);
  const view = new Uint8Array(backing.buffer, backing.byteOffset + 3, 5_000);
  assert.equal(sha256Hex(view), reference(view));
});

test("a finished hash says so instead of giving a wrong digest", () => {
  const hash = new Sha256().update(encode("abc"));
  assert.equal(toHex(hash.digest()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  assert.throws(() => hash.update(encode("more")), /finished/);
  assert.throws(() => hash.digest(), /finished/);
});

test("the length goes in as 64 bits: a total past 4 GiB of bits pads like node:crypto (the arithmetic, without the bytes)", () => {
  // Feeding gigabytes takes too long for a unit test; the padding of a 600 MB input already needs the high word of
  // the bit count (600 MB is 4.8e9 bits, past 2^32), so it is checked here against node:crypto with a repeated block.
  const block = randomBytes(1024 * 1024);
  const hash = new Sha256();
  const expected = createHash("sha256");
  for (let i = 0; i < 600; i++) {
    hash.update(block);
    expected.update(block);
  }
  assert.equal(hash.hex(), expected.digest("hex"));
});
