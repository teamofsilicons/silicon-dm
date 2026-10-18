/**
 * SHA-256 (FIPS 180-4), fed in pieces: what the browser computes over a file before it is sent, because Silicon
 * Briefcase wants a file's size and SHA-256 before its bytes (it checks the digest when they arrive). WebCrypto only
 * digests a whole buffer at once, which for a big file means holding all of it in memory, so the file is read in slices
 * and fed to this instead (in a Web Worker: ./hash.worker.ts, through ./hash.ts).
 *
 * Plain TypeScript rather than a WebAssembly package: nothing to add to the site's dependencies or to its script policy
 * (WebAssembly would need 'wasm-unsafe-eval'). tests/upload-sha256.test.ts checks it against the FIPS 180-2 examples,
 * the million-"a" vector and node:crypto over random lengths and random ways of cutting the input into pieces.
 */

const K = Int32Array.from([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

const INITIAL = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

/** Σ0 and Σ1 of the round function (rotations right by 2, 13, 22 and 6, 11, 25). */
const big0 = (x: number) => ((x >>> 2) | (x << 30)) ^ ((x >>> 13) | (x << 19)) ^ ((x >>> 22) | (x << 10));
const big1 = (x: number) => ((x >>> 6) | (x << 26)) ^ ((x >>> 11) | (x << 21)) ^ ((x >>> 25) | (x << 7));

/**
 * Compresses every whole 64-byte block of `view` from `offset` to `end` into `state`: the hot loop of a file's
 * preparation. The working variables are locals and the rounds are unrolled by eight, so they change roles instead of
 * being shifted along (the fastest of the shapes measured: about 330 MB/s in V8 on an M4 Pro, with node:crypto's
 * native code at 3 GB/s for scale).
 */
function compress(state: Int32Array, w: Int32Array, view: DataView, offset: number, end: number): void {
  let h0 = state[0]!, h1 = state[1]!, h2 = state[2]!, h3 = state[3]!, h4 = state[4]!, h5 = state[5]!, h6 = state[6]!, h7 = state[7]!;
  for (let p = offset; p + 64 <= end; p += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getInt32(p + i * 4);
    for (let i = 16; i < 64; i++) {
      const x = w[i - 15]!;
      const y = w[i - 2]!;
      const s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
      const s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
      w[i] = (w[i - 16]! + s0 + w[i - 7]! + s1) | 0;
    }
    let a = h0, b = h1, c = h2, d = h3, e = h4, f = h5, g = h6, h = h7;
    for (let i = 0; i < 64; i += 8) {
      let t1 = (h + big1(e) + (g ^ (e & (f ^ g))) + K[i]! + w[i]!) | 0;
      d = (d + t1) | 0;
      h = (t1 + big0(a) + ((a & b) | (c & (a | b)))) | 0;
      t1 = (g + big1(d) + (f ^ (d & (e ^ f))) + K[i + 1]! + w[i + 1]!) | 0;
      c = (c + t1) | 0;
      g = (t1 + big0(h) + ((h & a) | (b & (h | a)))) | 0;
      t1 = (f + big1(c) + (e ^ (c & (d ^ e))) + K[i + 2]! + w[i + 2]!) | 0;
      b = (b + t1) | 0;
      f = (t1 + big0(g) + ((g & h) | (a & (g | h)))) | 0;
      t1 = (e + big1(b) + (d ^ (b & (c ^ d))) + K[i + 3]! + w[i + 3]!) | 0;
      a = (a + t1) | 0;
      e = (t1 + big0(f) + ((f & g) | (h & (f | g)))) | 0;
      t1 = (d + big1(a) + (c ^ (a & (b ^ c))) + K[i + 4]! + w[i + 4]!) | 0;
      h = (h + t1) | 0;
      d = (t1 + big0(e) + ((e & f) | (g & (e | f)))) | 0;
      t1 = (c + big1(h) + (b ^ (h & (a ^ b))) + K[i + 5]! + w[i + 5]!) | 0;
      g = (g + t1) | 0;
      c = (t1 + big0(d) + ((d & e) | (f & (d | e)))) | 0;
      t1 = (b + big1(g) + (a ^ (g & (h ^ a))) + K[i + 6]! + w[i + 6]!) | 0;
      f = (f + t1) | 0;
      b = (t1 + big0(c) + ((c & d) | (e & (c | d)))) | 0;
      t1 = (a + big1(f) + (h ^ (f & (g ^ h))) + K[i + 7]! + w[i + 7]!) | 0;
      e = (e + t1) | 0;
      a = (t1 + big0(b) + ((b & c) | (d & (b | c)))) | 0;
    }
    h0 = (h0 + a) | 0;
    h1 = (h1 + b) | 0;
    h2 = (h2 + c) | 0;
    h3 = (h3 + d) | 0;
    h4 = (h4 + e) | 0;
    h5 = (h5 + f) | 0;
    h6 = (h6 + g) | 0;
    h7 = (h7 + h) | 0;
  }
  state[0] = h0;
  state[1] = h1;
  state[2] = h2;
  state[3] = h3;
  state[4] = h4;
  state[5] = h5;
  state[6] = h6;
  state[7] = h7;
}

export class Sha256 {
  private readonly state = Int32Array.from(INITIAL);
  private readonly schedule = new Int32Array(64);
  /** The bytes of a block not yet whole (fewer than 64). */
  private readonly pending = new Uint8Array(64);
  private readonly pendingView = new DataView(this.pending.buffer);
  private pendingLength = 0;
  /** Every byte fed so far (a JavaScript number is exact far beyond Briefcase's 5 TiB). */
  private total = 0;
  private finished = false;

  /** Feeds the next bytes (any length, any number of times). */
  update(bytes: Uint8Array): this {
    if (this.finished) throw new Error("This SHA-256 is finished: start a new one.");
    let offset = 0;
    const length = bytes.length;
    this.total += length;
    if (this.pendingLength > 0) {
      const take = Math.min(64 - this.pendingLength, length);
      this.pending.set(bytes.subarray(0, take), this.pendingLength);
      this.pendingLength += take;
      offset = take;
      if (this.pendingLength < 64) return this;
      compress(this.state, this.schedule, this.pendingView, 0, 64);
      this.pendingLength = 0;
    }
    const whole = offset + Math.floor((length - offset) / 64) * 64;
    if (whole > offset) compress(this.state, this.schedule, new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength), offset, whole);
    if (whole < length) {
      this.pending.set(bytes.subarray(whole), 0);
      this.pendingLength = length - whole;
    }
    return this;
  }

  /** The digest (32 bytes). The hash is finished after this. */
  digest(): Uint8Array {
    if (this.finished) throw new Error("This SHA-256 is finished: start a new one.");
    this.finished = true;
    const bits = this.total * 8;
    const tail = new Uint8Array(this.pendingLength < 56 ? 64 : 128);
    tail.set(this.pending.subarray(0, this.pendingLength));
    tail[this.pendingLength] = 0x80;
    const view = new DataView(tail.buffer);
    view.setUint32(tail.length - 8, Math.floor(bits / 0x1_0000_0000));
    view.setUint32(tail.length - 4, bits >>> 0);
    compress(this.state, this.schedule, view, 0, tail.length);
    const out = new Uint8Array(32);
    const outView = new DataView(out.buffer);
    for (let i = 0; i < 8; i++) outView.setInt32(i * 4, this.state[i]!);
    return out;
  }

  /** The digest as 64 lowercase hex characters (what Briefcase's reservation takes). */
  hex(): string {
    return toHex(this.digest());
  }
}

export function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

/** The SHA-256 of bytes held in memory, as hex. */
export const sha256Hex = (bytes: Uint8Array) => new Sha256().update(bytes).hex();
