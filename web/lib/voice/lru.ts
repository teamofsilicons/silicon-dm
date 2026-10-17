/**
 * A least-recently-used store bounded by bytes and by count, with pins: audio that is playing (or about to) is never
 * evicted, because its object URL must stay valid while the <audio> element reads it. `onEvict` releases what an entry
 * holds (the voice cache revokes the object URL). Pure, so it can be tested on its own.
 */
export interface LruOptions<T> {
  maxBytes: number;
  maxEntries: number;
  onEvict?: (key: string, value: T) => void;
}

interface Slot<T> {
  value: T;
  bytes: number;
}

export class Lru<T> {
  private readonly slots = new Map<string, Slot<T>>();
  private readonly pins = new Map<string, number>();
  private total = 0;

  constructor(private readonly options: LruOptions<T>) {}

  /** The value, marked as just used. */
  get(key: string): T | undefined {
    const slot = this.slots.get(key);
    if (!slot) return undefined;
    this.slots.delete(key);
    this.slots.set(key, slot);
    return slot.value;
  }

  /** The value without marking it used. */
  peek(key: string): T | undefined {
    return this.slots.get(key)?.value;
  }

  has(key: string): boolean {
    return this.slots.has(key);
  }

  /** Stores a value (replacing and releasing an older one under the key), then evicts down to the limits. */
  set(key: string, value: T, bytes: number): void {
    const old = this.slots.get(key);
    if (old) {
      this.slots.delete(key);
      this.total -= old.bytes;
      if (old.value !== value) this.options.onEvict?.(key, old.value);
    }
    const size = Math.max(0, Math.floor(bytes));
    this.slots.set(key, { value, bytes: size });
    this.total += size;
    // The entry just stored is about to be used: it never evicts itself (the store may run over while pins hold).
    this.trim(key);
  }

  delete(key: string): boolean {
    const slot = this.slots.get(key);
    if (!slot) return false;
    this.slots.delete(key);
    this.total -= slot.bytes;
    this.options.onEvict?.(key, slot.value);
    return true;
  }

  /** Deletes every entry whose key passes `test` (a voice change makes some audio stale). */
  deleteWhere(test: (key: string, value: T) => boolean): number {
    let count = 0;
    for (const [key, slot] of [...this.slots]) {
      if (!test(key, slot.value)) continue;
      if (this.pins.has(key)) continue;
      this.delete(key);
      count += 1;
    }
    return count;
  }

  /** Keeps `key` from eviction until as many unpins. */
  pin(key: string): void {
    this.pins.set(key, (this.pins.get(key) ?? 0) + 1);
  }

  unpin(key: string): void {
    const count = (this.pins.get(key) ?? 0) - 1;
    if (count > 0) this.pins.set(key, count);
    else this.pins.delete(key);
    this.trim();
  }

  clear(): void {
    for (const key of [...this.slots.keys()]) if (!this.pins.has(key)) this.delete(key);
  }

  /** Keys from the least to the most recently used. */
  keys(): string[] {
    return [...this.slots.keys()];
  }

  get size(): number {
    return this.slots.size;
  }

  get bytes(): number {
    return this.total;
  }

  private trim(keep?: string): void {
    if (this.total <= this.options.maxBytes && this.slots.size <= this.options.maxEntries) return;
    for (const key of [...this.slots.keys()]) {
      if (this.total <= this.options.maxBytes && this.slots.size <= this.options.maxEntries) return;
      if (this.pins.has(key) || key === keep) continue;
      this.delete(key);
    }
  }
}
