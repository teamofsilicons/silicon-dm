/**
 * WAV in the browser: a canonical 16-bit PCM file from decoded samples (voice notes play from it, so seeking and the
 * length are exact even for a MediaRecorder recording, which carries no duration), and the length of a WAV from its
 * header (the service's speech is 24 kHz mono 16-bit PCM). Pure.
 */

const HEADER_BYTES = 44;

/** A mono 16-bit PCM WAV of `samples` (floats from -1 to 1). */
export function encodeWav(samples: ArrayLike<number>, sampleRate: number): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(new ArrayBuffer(HEADER_BYTES + samples.length * 2));
  const view = new DataView(bytes.buffer);
  const text = (at: number, value: string) => {
    for (let index = 0; index < value.length; index++) view.setUint8(at + index, value.charCodeAt(index));
  };
  text(0, "RIFF");
  view.setUint32(4, 36 + samples.length * 2, true);
  text(8, "WAVE");
  text(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, "data");
  view.setUint32(40, samples.length * 2, true);
  for (let index = 0; index < samples.length; index++) {
    const value = Math.max(-1, Math.min(1, samples[index] ?? 0));
    view.setInt16(HEADER_BYTES + index * 2, value < 0 ? Math.round(value * 0x8000) : Math.round(value * 0x7fff), true);
  }
  return bytes;
}

export interface WavInfo {
  sampleRate: number;
  channels: number;
  bitsPerSample: number;
  dataBytes: number;
  durationMs: number;
}

/** What a WAV's header says (walking its chunks), or null when the bytes are not a PCM WAV. */
export function readWavInfo(input: ArrayBuffer | Uint8Array): WavInfo | null {
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
  if (bytes.length < 12) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const tag = (at: number) => String.fromCharCode(bytes[at]!, bytes[at + 1]!, bytes[at + 2]!, bytes[at + 3]!);
  if (tag(0) !== "RIFF" || tag(8) !== "WAVE") return null;
  let at = 12;
  let format: { sampleRate: number; channels: number; bitsPerSample: number } | null = null;
  while (at + 8 <= bytes.length) {
    const id = tag(at);
    const size = view.getUint32(at + 4, true);
    if (id === "fmt " && at + 24 <= bytes.length) {
      format = { channels: view.getUint16(at + 10, true), sampleRate: view.getUint32(at + 12, true), bitsPerSample: view.getUint16(at + 22, true) };
    } else if (id === "data" && format) {
      // A streamed WAV may say 0 or 0xFFFFFFFF here: then the data runs to the end of the file.
      const available = bytes.length - (at + 8);
      const dataBytes = size === 0 || size === 0xffffffff ? available : Math.min(size, available);
      const frameBytes = Math.max(1, (format.bitsPerSample / 8) * format.channels);
      const durationMs = format.sampleRate ? Math.round(((dataBytes / frameBytes) * 1000) / format.sampleRate) : 0;
      return { ...format, dataBytes, durationMs };
    }
    at += 8 + size + (size % 2);
  }
  return null;
}

/**
 * A tenth of a second of silence: played once inside a click so the browser lets later audio play. The page's
 * Content-Security-Policy allows media from blob: URLs (not data: ones), so the player makes one from these bytes.
 */
export function silentWav(): Uint8Array<ArrayBuffer> {
  return encodeWav(new Float32Array(800), 8000);
}
