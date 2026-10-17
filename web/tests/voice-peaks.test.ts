/** Waveform peaks (lib/voice/peaks.ts) and WAV (lib/voice/wav.ts): voice notes draw the way the service draws speech. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { barHeight, computePeaks, PEAK_COUNT, resamplePeaks, usablePeaks } from "../lib/voice/peaks";
import { encodeWav, readWavInfo, silentWav } from "../lib/voice/wav";

const tone = (length: number, amplitude: number) => Float32Array.from({ length }, (_, index) => (index % 2 ? -amplitude : amplitude));

test("64 RMS windows, relative to the loudest, from 0 to 100", () => {
  const quiet = tone(24_000, 0.1);
  const loud = tone(24_000, 0.4);
  const peaks = computePeaks(Float32Array.from([...quiet, ...loud]));
  assert.equal(peaks.length, PEAK_COUNT);
  assert.equal(Math.max(...peaks), 100, "the loudest window is 100");
  assert.equal(peaks[0], 25, "a quarter as loud is 25");
  assert.equal(peaks[63], 100);
  assert.ok(peaks.every(value => Number.isInteger(value) && value >= 0 && value <= 100));
});

test("silence is all zeros, and audio shorter than 64 samples pads with zeros", () => {
  assert.deepEqual(computePeaks(new Float32Array(48_000)), new Array(64).fill(0));
  assert.deepEqual(computePeaks(new Float32Array(0)), new Array(64).fill(0));
  const short = computePeaks(Float32Array.from([0.5, -0.5, 0.25]));
  assert.deepEqual(short.slice(0, 3), [100, 100, 50]);
  assert.ok(short.slice(3).every(value => value === 0));
});

test("matches the service's windows: ceil(length / 64) samples each, the last one shorter", () => {
  // 130 samples → windows of 3; window 43 holds the last sample alone.
  const samples = new Float32Array(130);
  samples[129] = 1;
  const peaks = computePeaks(samples);
  assert.equal(peaks[43], 100);
  assert.ok(peaks.slice(0, 43).every(value => value === 0));
});

test("several channels count as their mean energy", () => {
  const left = tone(6400, 0.5);
  const right = new Float32Array(6400);
  const mixed = computePeaks([left, right]);
  assert.ok(mixed.every(value => value === 100), "equal everywhere, so every window is the loudest");
});

test("resampling keeps the loudest of each span, and stretches when asked for more bars", () => {
  const peaks = Array.from({ length: 64 }, (_, index) => (index === 10 ? 90 : 5));
  const fewer = resamplePeaks(peaks, 16);
  assert.equal(fewer.length, 16);
  assert.equal(fewer[2], 90, "the loud word survives");
  assert.equal(Math.max(...fewer), 90);
  assert.deepEqual(resamplePeaks([10, 20], 4), [10, 10, 20, 20]);
  assert.deepEqual(resamplePeaks([], 3), [0, 0, 0]);
});

test("the service's peaks are used only when they are numbers", () => {
  assert.deepEqual(usablePeaks([0, 50, 120, -3]), [0, 50, 100, 0]);
  assert.equal(usablePeaks(null), null);
  assert.equal(usablePeaks([]), null);
  assert.equal(usablePeaks([1, "2"]), null);
  assert.equal(barHeight(0), 10);
  assert.equal(barHeight(55), 55);
});

test("a WAV made from samples says its own length", () => {
  const wav = encodeWav(new Float32Array(24_000 * 3), 24_000);
  assert.equal(wav.length, 44 + 24_000 * 3 * 2);
  const info = readWavInfo(wav);
  assert.deepEqual(info, { sampleRate: 24_000, channels: 1, bitsPerSample: 16, dataBytes: 144_000, durationMs: 3000 });
  assert.equal(readWavInfo(new Uint8Array([1, 2, 3])), null);
  assert.equal(readWavInfo(new TextEncoder().encode("RIFF....WAVEjunk")), null);
  // Clipped samples stay in range.
  const loud = encodeWav(Float32Array.from([2, -2]), 8000);
  const view = new DataView(loud.buffer);
  assert.equal(view.getInt16(44, true), 32767);
  assert.equal(view.getInt16(46, true), -32768);
  assert.equal(readWavInfo(silentWav())?.durationMs, 100, "a tenth of a second of silence unlocks audio");
});
