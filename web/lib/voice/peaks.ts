/**
 * Waveform peaks: bar heights from 0 to 100, the RMS loudness of equal windows of the audio relative to the loudest
 * window. It is the rule the DM service uses for rendered speech (src/domain/audio.rs `peaks`), so a Carbon's voice
 * note and a Silicon's spoken message draw the same way. Pure: decoding lives in lib/voice/sources.ts.
 */

/** How many peaks describe one recording (the service sends 64 for speech). */
export const PEAK_COUNT = 64;

/**
 * 64 (or `count`) values from 0 to 100 for mono or multichannel samples (each sample's energy is the mean of its
 * channels' squares). Silence is all zeros; audio shorter than `count` samples pads with zeros, as the service does.
 * Samples are floats from -1 to 1 (what the browser decodes); `floor` is the loudness below which a recording counts
 * as silence (the service's "RMS under 1" of 16-bit audio).
 */
export function computePeaks(channels: ArrayLike<number> | ArrayLike<number>[], options: { count?: number; floor?: number } = {}): number[] {
  const count = options.count ?? PEAK_COUNT;
  const floor = options.floor ?? 1 / 32768;
  const list: ArrayLike<number>[] = isChannelList(channels) ? channels : [channels];
  const length = list.reduce((most, channel) => Math.max(most, channel.length), 0);
  const rms = new Array<number>(count).fill(0);
  if (length > 0 && list.length > 0) {
    const window = Math.ceil(length / count);
    for (let index = 0; index < count; index++) {
      const start = index * window;
      if (start >= length) break;
      const end = Math.min(length, start + window);
      let energy = 0;
      for (let at = start; at < end; at++) {
        let sum = 0;
        for (const channel of list) {
          const value = channel[at] ?? 0;
          sum += value * value;
        }
        energy += sum / list.length;
      }
      rms[index] = Math.sqrt(energy / (end - start));
    }
  }
  const loudest = rms.reduce((most, value) => Math.max(most, value), 0);
  return rms.map(value => (loudest < floor ? 0 : Math.max(0, Math.min(100, Math.round((value / loudest) * 100)))));
}

function isChannelList(value: ArrayLike<number> | ArrayLike<number>[]): value is ArrayLike<number>[] {
  return Array.isArray(value) && value.length > 0 && typeof value[0] === "object";
}

/**
 * Fits peaks into `bars` bars (a narrow bubble draws fewer): each bar is the loudest peak of its span, so a short loud
 * word never disappears; more bars than peaks repeat the nearest peak.
 */
export function resamplePeaks(peaks: readonly number[], bars: number): number[] {
  const count = Math.max(1, Math.floor(bars));
  if (!peaks.length) return new Array<number>(count).fill(0);
  if (count === peaks.length) return [...peaks];
  const out: number[] = [];
  for (let index = 0; index < count; index++) {
    const start = Math.floor((index * peaks.length) / count);
    const end = Math.max(start + 1, Math.floor(((index + 1) * peaks.length) / count));
    let loudest = 0;
    for (let at = start; at < end && at < peaks.length; at++) loudest = Math.max(loudest, peaks[at] ?? 0);
    out.push(loudest);
  }
  return out;
}

/** The service's `speech.peaks` if they are usable (numbers from 0 to 100), else null. */
export function usablePeaks(value: unknown): number[] | null {
  if (!Array.isArray(value) || value.length === 0 || value.length > 1024) return null;
  const out: number[] = [];
  for (const item of value) {
    if (typeof item !== "number" || !Number.isFinite(item)) return null;
    out.push(Math.max(0, Math.min(100, item)));
  }
  return out;
}

/** What a bar's height is drawn as: never quite flat, so a quiet stretch still reads as part of the recording. */
export function barHeight(peak: number, minimum = 10): number {
  return Math.max(minimum, Math.min(100, peak));
}
