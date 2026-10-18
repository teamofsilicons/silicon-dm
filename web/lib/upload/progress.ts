/**
 * How an upload's progress reads: the bytes sent of the whole, the speed and the time left, for files big enough that a
 * percentage says too little ("1.2 of 4.8 GB · 23 MB/s · 3 min left"); a percentage for the rest ("Uploading 42%").
 * Pure, so tests/upload-progress.test.ts pins down the words and the arithmetic; the composer's tray uses it.
 */

/** From this size up, the tray says bytes, speed and time left instead of a percentage. */
export const BIG_FILE_BYTES = 32 * 1024 * 1024;

const KB = 1024;
const MB = 1024 * KB;
const GB = 1024 * MB;
const TB = 1024 * GB;

/** A size the way the tray says it: "640 KB", "12 MB", "4.8 GB", "1.2 TB" (one decimal under 10, none above). */
export function bytesWords(bytes: number): string {
  const [unit, size] = bytes >= TB ? ["TB", TB] as const : bytes >= GB ? ["GB", GB] as const : bytes >= MB ? ["MB", MB] as const : bytes >= KB ? ["KB", KB] as const : ["bytes", 1] as const;
  return `${amount(bytes / size, unit === "bytes")} ${unit}`;
}

function amount(value: number, whole: boolean): string {
  if (whole || value >= 10) return String(Math.round(value));
  return String(Number(value.toFixed(1)));
}

/**
 * "0.5 of 4.8 GB": the part in the whole's unit, so the two read as one quantity; "30 MB of 2.3 GB" while the part is
 * still under a tenth of that unit (a "0 of 2.3 GB" that is moving would look stuck).
 */
export function partWords(sent: number, total: number): string {
  const [unit, size] = total >= TB ? ["TB", TB] as const : total >= GB ? ["GB", GB] as const : total >= MB ? ["MB", MB] as const : ["KB", KB] as const;
  const part = sent / size;
  const shown = part >= 10 ? Math.floor(part) : Math.floor(part * 10) / 10;
  if (shown === 0 && sent > 0) return `${bytesWords(sent)} of ${amount(total / size, false)} ${unit}`;
  return `${shown} of ${amount(total / size, false)} ${unit}`;
}

/** "23 MB/s", "640 KB/s". */
export const rateWords = (bytesPerSecond: number) => `${bytesWords(Math.max(0, bytesPerSecond))}/s`;

/** "45 s left", "3 min left", "1 h 20 min left", "2 days left" (rounded up, so it never promises too soon). */
export function leftWords(seconds: number): string {
  const s = Math.max(1, Math.ceil(seconds));
  if (s < 60) return `${s} s left`;
  const minutes = Math.ceil(s / 60);
  if (minutes < 60) return `${minutes} min left`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours} h${minutes % 60 ? ` ${minutes % 60} min` : ""} left`;
  return `${Math.ceil(hours / 24)} days left`;
}

/**
 * The speed of a transfer, smoothed so the words do not jump around: an exponentially weighted rate over the samples
 * (`note(sent, now)`), with the time left from it. No estimate until two seconds of sending have been seen.
 */
export class RateMeter {
  private first: { sent: number; at: number } | null = null;
  private last: { sent: number; at: number } | null = null;
  private smoothed = 0;

  /** Starts again from the first byte (a resend after a lost connection). */
  reset(): void {
    this.first = null;
    this.last = null;
    this.smoothed = 0;
  }

  note(sent: number, now: number): void {
    if (!this.first || !this.last || sent < this.last.sent) {
      this.first = { sent, at: now };
      this.last = { sent, at: now };
      this.smoothed = 0;
      return;
    }
    const elapsed = (now - this.last.at) / 1000;
    if (elapsed < 0.25) return;
    const rate = (sent - this.last.sent) / elapsed;
    // About a five-second memory: steady enough to read, quick enough to follow a real change.
    const weight = Math.min(1, elapsed / 5);
    this.smoothed = this.smoothed ? this.smoothed + (rate - this.smoothed) * weight : rate;
    this.last = { sent, at: now };
  }

  /** Bytes per second, or null before there is enough to say. */
  rate(): number | null {
    if (!this.first || !this.last || this.last.at - this.first.at < 2000 || this.smoothed <= 0) return null;
    return this.smoothed;
  }

  /** Seconds left for `total` bytes, or null when unknown. */
  left(total: number): number | null {
    const rate = this.rate();
    if (!rate || !this.last) return null;
    return Math.max(0, total - this.last.sent) / rate;
  }
}

export interface ProgressState {
  stage: "queued" | "preparing" | "sending" | "saving";
  /** Bytes hashed (preparing) or sent (sending). */
  done: number;
  total: number;
  /** Bytes per second while sending, when known. */
  rate?: number | null;
  /** Seconds left while sending, when known. */
  left?: number | null;
  /** Saving: Briefcase is storing a large file, which takes a while (the website said so). */
  long?: boolean;
}

/** The line under a file in the tray while it uploads. */
export function progressWords(state: ProgressState): string {
  const percent = state.total > 0 ? Math.min(100, Math.floor((state.done / state.total) * 100)) : 0;
  switch (state.stage) {
    case "queued":
      return "Waiting to upload…";
    case "preparing":
      // A file that takes minutes to prepare says how long, once that is known.
      return state.left !== null && state.left !== undefined && state.left >= 10 ? `Preparing… ${percent}% · ${leftWords(state.left)}` : `Preparing… ${percent}%`;
    case "saving":
      return state.long ? "Saving… this can take a few minutes" : "Saving…";
    case "sending": {
      if (state.total < BIG_FILE_BYTES) return `Uploading ${percent}%`;
      const parts = [partWords(state.done, state.total)];
      if (state.rate) parts.push(rateWords(state.rate));
      if (state.left !== null && state.left !== undefined && state.rate) parts.push(leftWords(state.left));
      return parts.join(" · ");
    }
  }
}
