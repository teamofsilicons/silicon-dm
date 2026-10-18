#!/usr/bin/env node
// The Deepgram stand-in of the chat stack (127.0.0.1:4540): the two Deepgram APIs DM's service calls, answered locally
// and deterministically, so speech messages and voice-note transcription work end to end without the network.
//
//   ws://127.0.0.1:4540/v1/agent/converse   the Voice Agent protocol, used for ElevenLabs speech:
//       Welcome -> Settings -> SettingsApplied (or Error INVALID_SETTINGS for an audio setup ElevenLabs cannot deliver:
//       not linear16, a container, a bad sample rate) -> InjectAgentMessage -> ConversationText (role assistant),
//       AgentStartedSpeaking, binary linear16 PCM at the requested rate (about 65 ms per spoken character, a soft
//       voiced tone whose loudness follows the words, so waveforms look real), AgentAudioDone. A voice_id outside
//       the catalog (DM_VOICES, default the five voices of the spec) and a script of cues only answer Error
//       FAILED_TO_SPEAK. KeepAlive is accepted. Cues behave as measured with the real ElevenLabs v4: not read aloud,
//       pause cues add silence, reaction cues ([laughs], [sighs]…) add a short sound, very long cues are read.
//   POST /v1/listen                          pre-recorded transcription: Deepgram's JSON shape with a deterministic
//       transcript ("This is a voice note from the stand-in.", plus its length for notes of 4 s or more; "" for
//       silence or under 0.3 s) and metadata.duration read from the audio (WAV, Ogg, WebM, MP4/M4A, FLAC, MP3 and
//       AAC/ADTS from their own headers; anything else estimated from its size). Every answer carries
//       `x-dm-stand-in: deepgram`.
//
// Any non-empty key is accepted (Authorization: Token|Bearer, or the WebSocket subprotocols ['token', key]); a
// missing one is refused like Deepgram does (401 INVALID_AUTH). Test controls, for provoking failures:
//   GET /_health   GET|DELETE /_requests (the last 300 requests: method, path, status, sizes; never bodies or keys)
//   POST /_faults {"target":"listen","status":503,"count":1,"delay_ms":0}
//   POST /_faults {"target":"agent","code":"FAILED_TO_SPEAK"|"INVALID_SETTINGS"|"<any>","count":1}
//   POST /_faults {"target":"agent","mode":"reject"|"no_welcome"|"hang"|"close_mid_audio","status":503,"count":1}
//   GET|DELETE /_faults
// Requests are logged (never bodies, text or keys) to DEEPGRAM_STANDIN_LOG (default .dev/logs/deepgram.log).
// Settings: --port (4540), DEEPGRAM_STANDIN_FIRST_AUDIO_MS (120), DEEPGRAM_STANDIN_CHUNK_INTERVAL_MS (2).
import { createHash, randomUUID } from 'node:crypto';
import { appendFileSync, mkdirSync } from 'node:fs';
import http from 'node:http';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { acceptWebSocket, refuseUpgrade } from './lib/ws.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const portIndex = process.argv.indexOf('--port');
const PORT = Number(portIndex >= 0 ? process.argv[portIndex + 1] : process.env.DEEPGRAM_STANDIN_PORT || 4540);
const LOG = process.env.DEEPGRAM_STANDIN_LOG || join(resolve(HERE, '..', '..'), '.dev', 'logs', 'deepgram.log');
const FIRST_AUDIO_MS = Number(process.env.DEEPGRAM_STANDIN_FIRST_AUDIO_MS ?? 120);
const CHUNK_INTERVAL_MS = Number(process.env.DEEPGRAM_STANDIN_CHUNK_INTERVAL_MS ?? 2);
const MS_PER_CHARACTER = 65;
const MAX_LISTEN_BYTES = 2 * 1024 * 1024 * 1024;
const KEEP_FOR_PARSING = 256 * 1024 * 1024;
const IDLE_CLOSE_MS = 30_000;

/** The ElevenLabs voices of the spec (§3); DM_VOICES (the same JSON as DM's setting) replaces them. */
const DEFAULT_VOICES = [
  { key: 'mark', voice_id: 'UgBBYS2sOqTuMpoF3BR0' },
  { key: 'brittney', voice_id: 'kPzsL2i3teMYv0FxEYQ6' },
  { key: 'jamal', voice_id: '6OzrBCQf8cjERkYgzSg8' },
  { key: 'jessa', voice_id: 'yj30vwTGJxSHezdAGsv9' },
  { key: 'hope', voice_id: 'tnSpp4vdxKPjI9w0GnoV' },
];
function voices() {
  try {
    const parsed = process.env.DM_VOICES ? JSON.parse(process.env.DM_VOICES) : null;
    if (Array.isArray(parsed) && parsed.every((voice) => typeof voice?.voice_id === 'string')) return parsed;
  } catch {
    // fall back to the default catalog
  }
  return DEFAULT_VOICES;
}
const VOICE_IDS = new Set(voices().map((voice) => voice.voice_id));
const ELEVEN_MODELS = new Set(['eleven_v4', 'eleven_v3', 'eleven_multilingual_v2', 'eleven_turbo_v2_5', 'eleven_flash_v2_5']);

// ------------------------------------------------------------------------------------------------ log and faults

const recent = [];
function record(entry) {
  const line = { at: new Date().toISOString(), ...entry };
  recent.unshift(line);
  if (recent.length > 300) recent.length = 300;
  try {
    mkdirSync(dirname(LOG), { recursive: true });
    appendFileSync(LOG, `${JSON.stringify(line)}\n`, { mode: 0o600 });
  } catch {
    // logging must never break an answer
  }
}

let faults = [];
/** Takes the next fault for `target`, counting it down. */
function takeFault(target) {
  const index = faults.findIndex((fault) => fault.target === target && fault.remaining > 0);
  if (index < 0) return null;
  const fault = faults[index];
  fault.remaining -= 1;
  if (fault.remaining <= 0) faults.splice(index, 1);
  return fault;
}

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

function sendJson(res, status, value, headers = {}) {
  const body = JSON.stringify(value);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body), 'x-dm-stand-in': 'deepgram', ...headers });
  res.end(body);
}

function deepgramError(res, status, code, message, requestId = randomUUID()) {
  sendJson(res, status, { err_code: code, err_msg: message, request_id: requestId }, { 'dg-request-id': requestId });
}

function authorized(headers) {
  return /^(token|bearer)\s+\S+/i.test(String(headers.authorization || '').trim());
}

async function readBody(req, limit) {
  const hash = createHash('sha256');
  const kept = [];
  let size = 0;
  let keptBytes = 0;
  for await (const chunk of req) {
    size += chunk.length;
    if (size > limit) throw Object.assign(new Error('too large'), { tooLarge: true });
    hash.update(chunk);
    if (keptBytes < KEEP_FOR_PARSING) {
      kept.push(chunk);
      keptBytes += chunk.length;
    }
  }
  return { bytes: Buffer.concat(kept), size, sha256: hash.digest('hex'), complete: keptBytes >= size };
}

// ------------------------------------------------------------------------------------------------ audio durations

/** Seconds of audio, read from the container where it says so, else estimated from the size. */
function audioInfo(bytes, size, contentType) {
  const type = String(contentType || '').split(';')[0].trim().toLowerCase();
  const ascii = (offset, length) => bytes.subarray(offset, offset + length).toString('latin1');
  if (bytes.length >= 12 && ascii(0, 4) === 'RIFF' && ascii(8, 4) === 'WAVE') return wavInfo(bytes);
  if (bytes.length >= 4 && ascii(0, 4) === 'OggS') return { duration: oggDuration(bytes), format: 'ogg' };
  if (bytes.length >= 4 && bytes.readUInt32BE(0) === 0x1a45dfa3) return { duration: webmDuration(bytes), format: 'webm' };
  if (bytes.length >= 4 && ascii(0, 4) === 'fLaC') return { duration: flacDuration(bytes), format: 'flac' };
  if (bytes.length >= 12 && ascii(4, 4) === 'ftyp') return { duration: mp4Duration(bytes), format: 'mp4' };
  const mp3 = mp3Duration(bytes);
  if (mp3 !== null) return { duration: mp3, format: 'mp3' };
  const adts = adtsDuration(bytes);
  if (adts !== null) return { duration: adts, format: 'aac' };
  // Anything else: estimated from the size at a typical bitrate.
  const bitrate = type.includes('aac') ? 96_000 : type.includes('mpeg') || type.includes('mp3') ? 128_000 : 64_000;
  return { duration: (size * 8) / bitrate, format: type || 'unknown', estimated: true };
}

function wavInfo(bytes) {
  let offset = 12;
  let format = null;
  while (offset + 8 <= bytes.length) {
    const id = bytes.subarray(offset, offset + 4).toString('latin1');
    const length = bytes.readUInt32LE(offset + 4);
    const body = offset + 8;
    if (id === 'fmt ' && body + 16 <= bytes.length) {
      format = { channels: bytes.readUInt16LE(body + 2), rate: bytes.readUInt32LE(body + 4), bits: bytes.readUInt16LE(body + 14), code: bytes.readUInt16LE(body) };
    } else if (id === 'data' && format) {
      const available = Math.min(length, bytes.length - body);
      const frameBytes = Math.max(1, format.channels * (format.bits / 8));
      const declared = length === 0xffffffff || length === 0 ? available : length;
      let peak = 0;
      if (format.code === 1 && format.bits === 16) {
        for (let i = body; i + 1 < body + available; i += 2) peak = Math.max(peak, Math.abs(bytes.readInt16LE(i)));
      } else {
        peak = 32767;
      }
      return { duration: declared / frameBytes / format.rate, format: 'wav', silent: peak < 330 };
    }
    offset = body + length + (length % 2);
  }
  return { duration: 0, format: 'wav', silent: true };
}

function oggDuration(bytes) {
  // The last page's granule position, in 48 kHz samples for Opus (minus the pre-skip), else the Vorbis rate.
  let last = -1;
  for (let i = bytes.length - 27; i >= 0; i--) {
    if (bytes[i] === 0x4f && bytes[i + 1] === 0x67 && bytes[i + 2] === 0x67 && bytes[i + 3] === 0x53) {
      last = i;
      break;
    }
  }
  if (last < 0) return 0;
  const granule = Number(bytes.readBigUInt64LE(last + 6));
  const head = bytes.indexOf('OpusHead');
  if (head >= 0) return Math.max(0, granule - bytes.readUInt16LE(head + 10)) / 48_000;
  const vorbis = bytes.indexOf('\x01vorbis');
  const rate = vorbis >= 0 ? bytes.readUInt32LE(vorbis + 12) : 48_000;
  return granule / rate;
}

/** WebM/Matroska: the Duration of the segment info, or (MediaRecorder writes none) the last block's time. */
function webmDuration(bytes) {
  const readVint = (offset, keepMarker) => {
    const first = bytes[offset];
    if (first === undefined) return null;
    let length = 1;
    while (length <= 8 && !(first & (0x80 >> (length - 1)))) length++;
    if (length > 8 || offset + length > bytes.length) return null;
    let value = keepMarker ? first : first & (0xff >> length);
    let unknown = (first & (0xff >> length)) === (0xff >> length);
    for (let i = 1; i < length; i++) {
      value = value * 256 + bytes[offset + i];
      if (bytes[offset + i] !== 0xff) unknown = false;
    }
    return { value, length, unknown: !keepMarker && unknown };
  };
  const MASTERS = new Set([0x18538067, 0x1549a966, 0x1f43b675, 0xa0]);
  let scale = 1_000_000;
  let declared = null;
  let clusterTime = 0;
  let lastMs = 0;
  let offset = 0;
  while (offset < bytes.length) {
    const id = readVint(offset, true);
    if (!id) break;
    const size = readVint(offset + id.length, false);
    if (!size) break;
    const body = offset + id.length + size.length;
    const readUint = () => {
      let value = 0;
      for (let i = 0; i < size.value && body + i < bytes.length; i++) value = value * 256 + bytes[body + i];
      return value;
    };
    if (MASTERS.has(id.value)) {
      offset = body; // descend: segment, info, cluster, block group (also when their size is unknown)
      continue;
    }
    if (id.value === 0x2ad7b1) scale = readUint();
    else if (id.value === 0x4489) declared = size.value === 4 ? bytes.readFloatBE(body) : size.value === 8 ? bytes.readDoubleBE(body) : null;
    else if (id.value === 0xe7) clusterTime = readUint();
    else if (id.value === 0xa3 || id.value === 0xa1) {
      const track = readVint(body, false);
      if (track && body + track.length + 2 <= bytes.length) lastMs = Math.max(lastMs, (clusterTime + bytes.readInt16BE(body + track.length)) * scale / 1_000_000);
    }
    if (size.unknown) break;
    offset = body + size.value;
  }
  if (declared && declared > 0) return (declared * scale) / 1e9;
  return lastMs > 0 ? (lastMs + 20) / 1000 : 0;
}

/** MPEG audio (MP3): walks the frames after an ID3v2 tag and adds up their samples; null when it is not MP3. */
function mp3Duration(bytes) {
  let offset = 0;
  if (bytes.length >= 10 && bytes.subarray(0, 3).toString('latin1') === 'ID3') {
    offset = 10 + ((bytes[6] & 0x7f) << 21 | (bytes[7] & 0x7f) << 14 | (bytes[8] & 0x7f) << 7 | (bytes[9] & 0x7f));
  }
  const BITRATES = {
    1: [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    2: [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
  };
  const RATES = { 3: [44100, 48000, 32000], 2: [22050, 24000, 16000], 0: [11025, 12000, 8000] };
  let seconds = 0;
  let frames = 0;
  while (offset + 4 <= bytes.length) {
    if (bytes[offset] !== 0xff || (bytes[offset + 1] & 0xe0) !== 0xe0) {
      if (frames === 0 && offset < 4096) {
        offset += 1;
        continue;
      }
      break;
    }
    const version = (bytes[offset + 1] >> 3) & 0x03; // 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5
    const layer = (bytes[offset + 1] >> 1) & 0x03; // 1 = Layer III
    const bitrateIndex = bytes[offset + 2] >> 4;
    const rateIndex = (bytes[offset + 2] >> 2) & 0x03;
    if (version === 1 || layer !== 1 || bitrateIndex === 0 || bitrateIndex === 15 || rateIndex === 3) break;
    const bitrate = BITRATES[version === 3 ? 1 : 2][bitrateIndex] * 1000;
    const rate = RATES[version][rateIndex];
    const samples = version === 3 ? 1152 : 576;
    const length = Math.floor((samples / 8) * bitrate / rate) + ((bytes[offset + 2] >> 1) & 0x01);
    if (length <= 4) break;
    seconds += samples / rate;
    frames += 1;
    offset += length;
  }
  return frames >= 2 ? seconds : null;
}

/** AAC in ADTS frames (1024 samples each); null when it is not ADTS. */
function adtsDuration(bytes) {
  const RATES = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
  let offset = 0;
  let seconds = 0;
  let frames = 0;
  while (offset + 7 <= bytes.length && bytes[offset] === 0xff && (bytes[offset + 1] & 0xf6) === 0xf0) {
    const rate = RATES[(bytes[offset + 2] >> 2) & 0x0f];
    const length = ((bytes[offset + 3] & 0x03) << 11) | (bytes[offset + 4] << 3) | (bytes[offset + 5] >> 5);
    const blocks = (bytes[offset + 6] & 0x03) + 1;
    if (!rate || length < 7) break;
    seconds += (1024 * blocks) / rate;
    frames += 1;
    offset += length;
  }
  return frames >= 2 ? seconds : null;
}

function flacDuration(bytes) {
  if (bytes.length < 26) return 0;
  const info = 8; // after "fLaC" and the first block header: STREAMINFO
  const rate = (bytes[info + 10] << 12) | (bytes[info + 11] << 4) | (bytes[info + 12] >> 4);
  const total = ((bytes[info + 13] & 0x0f) * 2 ** 32) + bytes.readUInt32BE(info + 14);
  return rate ? total / rate : 0;
}

function mp4Duration(bytes) {
  const find = (start, end, type) => {
    let offset = start;
    while (offset + 8 <= end) {
      let size = bytes.readUInt32BE(offset);
      const name = bytes.subarray(offset + 4, offset + 8).toString('latin1');
      let header = 8;
      if (size === 1 && offset + 16 <= end) {
        size = Number(bytes.readBigUInt64BE(offset + 8));
        header = 16;
      } else if (size === 0) {
        size = end - offset;
      }
      if (size < header) return null;
      if (name === type) return { start: offset + header, end: Math.min(end, offset + size) };
      offset += size;
    }
    return null;
  };
  const moov = find(0, bytes.length, 'moov');
  const mvhd = moov && find(moov.start, moov.end, 'mvhd');
  if (!mvhd) return 0;
  const version = bytes[mvhd.start];
  if (version === 1) return Number(bytes.readBigUInt64BE(mvhd.start + 24)) / bytes.readUInt32BE(mvhd.start + 20);
  return bytes.readUInt32BE(mvhd.start + 16) / bytes.readUInt32BE(mvhd.start + 12);
}

// ------------------------------------------------------------------------------------------------ /v1/listen

const SENTENCE = 'This is a voice note from the stand-in.';
const NUMBERS = ['zero', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight', 'nine', 'ten', 'eleven', 'twelve', 'thirteen', 'fourteen', 'fifteen', 'sixteen', 'seventeen', 'eighteen', 'nineteen', 'twenty'];

function transcriptFor(seconds, smart) {
  if (seconds < 4) return SENTENCE;
  const whole = Math.round(seconds);
  const spoken = smart || whole > 20 ? String(whole) : NUMBERS[whole];
  return `${SENTENCE} It is ${spoken} seconds long.`;
}

function words(transcript, duration) {
  const parts = transcript.split(/\s+/).filter(Boolean);
  const span = Math.max(duration - 0.16, 0.1);
  const step = span / Math.max(parts.length, 1);
  return parts.map((part, index) => ({
    word: part.toLowerCase().replace(/[^a-z0-9'-]/g, ''),
    start: Number((0.08 + index * step).toFixed(3)),
    end: Number((0.08 + (index + 0.85) * step).toFixed(3)),
    confidence: 0.99,
    punctuated_word: part,
  }));
}

async function listen(req, res, url) {
  const started = Date.now();
  const requestId = randomUUID();
  const query = Object.fromEntries(['model', 'language', 'detect_language', 'smart_format', 'punctuate', 'mip_opt_out', 'paragraphs'].map((key) => [key, url.searchParams.get(key)]).filter(([, value]) => value !== null));
  const finish = (status, extra = {}) => record({ kind: 'listen', method: 'POST', path: url.pathname, query, status, request_id: requestId, ms: Date.now() - started, ...extra });
  if (!authorized(req.headers)) {
    req.resume();
    deepgramError(res, 401, 'INVALID_AUTH', 'Invalid credentials.', requestId);
    return finish(401);
  }
  const fault = takeFault('listen');
  if (fault?.delay_ms) await sleep(fault.delay_ms);
  if (fault?.status) {
    req.resume();
    deepgramError(res, fault.status, fault.code || (fault.status === 429 ? 'TOO_MANY_REQUESTS' : 'INTERNAL_SERVER_ERROR'), fault.message || 'A fault set with POST /_faults.', requestId);
    return finish(fault.status, { fault: true });
  }
  const contentType = String(req.headers['content-type'] || '');
  let body;
  try {
    body = await readBody(req, MAX_LISTEN_BYTES);
  } catch (error) {
    deepgramError(res, 413, 'PAYLOAD_TOO_LARGE', 'The audio is larger than the stand-in accepts (2 GiB).', requestId);
    return finish(413);
  }
  if (contentType.startsWith('application/json')) {
    deepgramError(res, 400, 'Bad Request', 'The stand-in transcribes uploaded audio only; a remote audio URL was sent.', requestId);
    return finish(400, { bytes_in: body.size });
  }
  if (body.size === 0) {
    deepgramError(res, 400, 'Bad Request', 'Bad Request: failed to process audio: corrupt or unsupported data', requestId);
    return finish(400, { bytes_in: 0 });
  }
  const info = audioInfo(body.bytes, body.size, contentType);
  const duration = Number((info.duration || 0).toFixed(3));
  const smart = query.smart_format === 'true';
  const transcript = info.silent || duration < 0.3 ? '' : transcriptFor(duration, smart);
  const modelName = query.model || 'base';
  const modelId = createHash('sha256').update(modelName).digest('hex').slice(0, 32).replace(/(.{8})(.{4})(.{4})(.{4})(.{12})/, '$1-$2-$3-$4-$5');
  const alternative = { transcript, confidence: transcript ? 0.98 : 0, words: transcript ? words(transcript, duration) : [] };
  if (smart || query.paragraphs === 'true') {
    alternative.paragraphs = transcript
      ? { transcript: `\n${transcript}`, paragraphs: [{ sentences: transcript.split(/(?<=\.)\s+/).map((text, index, all) => ({ text, start: Number((index * duration / all.length).toFixed(3)), end: Number(((index + 1) * duration / all.length).toFixed(3)) })), num_words: alternative.words.length, start: 0.08, end: duration }] }
      : { transcript: '', paragraphs: [] };
  }
  const channel = { alternatives: [alternative] };
  if (query.detect_language === 'true') Object.assign(channel, { detected_language: 'en', language_confidence: 0.99 });
  sendJson(res, 200, {
    metadata: {
      transaction_key: 'deprecated', request_id: requestId, sha256: body.sha256, created: new Date().toISOString(), duration, channels: 1,
      models: [modelId], model_info: { [modelId]: { name: `general-${modelName}`, version: 'stand-in', arch: modelName } },
    },
    results: { channels: [channel] },
  }, { 'dg-request-id': requestId });
  return finish(200, { bytes_in: body.size, content_type: contentType.split(';')[0], format: info.format, audio_seconds: duration, estimated: Boolean(info.estimated), empty: !transcript });
}

// ------------------------------------------------------------------------------------------------ speech audio

/**
 * How ElevenLabs v4 performs a script through the Voice Agent (as measured on 2026-10-17, progress/cli.md): cues in
 * square brackets are not read aloud; pause cues take time ([short pause] about 0.8 s, [pause] 1 s, [long pause]
 * 1.7 s, "(pause)" 1.3 s); reaction cues ([laughs], [sighs], [clears throat], "(sighs)"…) add about 0.7 s of sound; an
 * instruction cue longer than 16 words IS read aloud; *laughs* in asterisks is dropped. Returns the timeline and the
 * spoken words; a script with no words at all fails like the real one (FAILED_TO_SPEAK).
 */
const PAUSE_CUES = { 'short pause': 800, pause: 1000, 'long pause': 1700 };
const SOUND_CUE = /^(laughs?|laughing|chuckles?|giggles?|sighs?|clears throat|exhales?( sharply)?|inhales?( deeply)?|gasps?|coughs?|sniffs?|snorts?)$/;
function perform(text) {
  const segments = [];
  let speech = '';
  const flush = () => {
    const words = speech.replace(/\s+/g, ' ').trim();
    if (words) segments.push({ kind: 'speech', text: words });
    speech = '';
  };
  for (const token of String(text).split(/(\[[^\]]*\]|\([^()]{1,40}\)|\*[^*\s][^*]{0,38}\*)/)) {
    if (!token) continue;
    const bracket = token.startsWith('[') && token.endsWith(']');
    const paren = token.startsWith('(') && token.endsWith(')');
    if (bracket || paren) {
      const cue = token.slice(1, -1).trim().toLowerCase().replace(/^\/\s*/, '');
      if (bracket && cue.split(/\s+/).length > 16) {
        speech += ` ${token.slice(1, -1)} `;
      } else if (bracket && PAUSE_CUES[cue] !== undefined) {
        flush();
        segments.push({ kind: 'pause', ms: PAUSE_CUES[cue] });
      } else if (paren && cue === 'pause') {
        flush();
        segments.push({ kind: 'pause', ms: 1300 });
      } else if (SOUND_CUE.test(cue)) {
        flush();
        segments.push({ kind: 'sound', ms: 700 });
      } else if (paren) {
        speech += token; // other parentheses are read like any words
      } else {
        speech += ' ';
      }
      continue;
    }
    if (token.length > 2 && token.startsWith('*') && token.endsWith('*')) {
      speech += ' ';
      continue;
    }
    speech += token;
  }
  flush();
  return { segments, spoken: segments.filter((s) => s.kind === 'speech').map((s) => s.text).join(' ') };
}

function spokenText(text) {
  return perform(text).spoken;
}

function hashOf(text) {
  return createHash('sha256').update(text).digest().readUInt32BE(0);
}

/**
 * linear16 mono PCM: about 65 ms per spoken character (spaces and punctuation included), plus the pauses and sounds
 * of the cues. Each word is a soft voiced tone (a fundamental with two harmonics and a little vibrato) under one bump
 * per syllable, with short gaps between words and longer ones after punctuation; a reaction is a breathy burst. The
 * pitch depends on the voice, so two voices look and sound different. Deterministic.
 */
function synthesize(text, voiceId, rate) {
  const { segments } = perform(text);
  const lengths = segments.map((segment) => Math.max(1, Math.round((rate * (segment.kind === 'speech' ? [...segment.text].length * MS_PER_CHARACTER : segment.ms)) / 1000)));
  const total = lengths.reduce((a, b) => a + b, 0) || 1;
  const pcm = Buffer.alloc(total * 2);
  const base = 105 + (hashOf(voiceId) % 140);
  let offset = 0;
  segments.forEach((segment, segmentIndex) => {
    const length = lengths[segmentIndex];
    if (segment.kind === 'sound') {
      let seed = hashOf(`${voiceId}:${segmentIndex}`) || 1;
      for (let i = 0; i < length; i++) {
        seed = (seed * 1103515245 + 12345) >>> 0;
        const noise = (seed / 2 ** 32) * 2 - 1;
        const envelope = Math.sin((Math.PI * i) / length) * (0.6 + 0.4 * Math.sin((2 * Math.PI * 4 * i) / rate) ** 2);
        pcm.writeInt16LE(Math.round(noise * envelope * 0.12 * 32767), (offset + i) * 2);
      }
    } else if (segment.kind === 'speech') {
      const parts = segment.text.split(' ').filter(Boolean);
      const weights = parts.map((word) => word.length + 1 + (/[.!?…]$/.test(word) ? 4 : /[,;:]$/.test(word) ? 2 : 0));
      const sum = weights.reduce((a, b) => a + b, 0) || 1;
      let cursor = 0;
      parts.forEach((word, index) => {
        const span = Math.floor((weights[index] / sum) * length);
        const voiced = Math.max(1, Math.floor(span * (word.length / weights[index])));
        const syllables = Math.max(1, Math.round(word.replace(/[^a-z]/gi, '').length / 3));
        const loudness = 0.18 + ((hashOf(`${voiceId}:${word}:${index}`) % 100) / 100) * 0.14;
        const pitch = base * (0.92 + ((hashOf(word) % 17) / 100)) * (/\?$/.test(word) ? 1.18 : 1);
        for (let i = 0; i < voiced && cursor + i < length; i++) {
          const t = (offset + cursor + i) / rate;
          const within = (i / voiced) * syllables;
          const envelope = Math.sin(Math.PI * (within % 1)) ** 0.8 * Math.min(1, i / (rate * 0.01), (voiced - i) / (rate * 0.01));
          const f = pitch * (1 + 0.012 * Math.sin(2 * Math.PI * 5.5 * t));
          const tone = (Math.sin(2 * Math.PI * f * t) + 0.45 * Math.sin(4 * Math.PI * f * t) + 0.2 * Math.sin(6 * Math.PI * f * t)) / 1.65;
          pcm.writeInt16LE(Math.round(Math.max(-1, Math.min(1, tone * envelope * loudness)) * 32767), (offset + cursor + i) * 2);
        }
        cursor += span;
      });
    }
    offset += length;
  });
  return pcm;
}

// ------------------------------------------------------------------------------------------------ the Voice Agent

function validateSettings(settings) {
  const input = settings?.audio?.input;
  const output = settings?.audio?.output ?? {};
  if (!input || typeof input !== 'object') return 'audio.input is required: {"encoding":"linear16","sample_rate":24000}.';
  if (output.encoding !== undefined && output.encoding !== 'linear16') return `audio.output.encoding "${output.encoding}" cannot be delivered for ElevenLabs speech; use linear16.`;
  if (output.container !== undefined && output.container !== 'none') return `audio.output.container "${output.container}": ElevenLabs speech comes only as raw linear16 (container "none").`;
  const rate = output.sample_rate ?? 24000;
  if (!Number.isInteger(rate) || rate < 8000 || rate > 48000) return `audio.output.sample_rate ${JSON.stringify(output.sample_rate)} must be a whole number of hertz from 8000 to 48000.`;
  const provider = settings?.agent?.speak?.provider;
  if (!provider || typeof provider !== 'object') return 'agent.speak.provider is required.';
  if (provider.type !== 'eleven_labs' && provider.type !== 'deepgram') return `agent.speak.provider.type "${provider.type}" is not a provider the stand-in speaks (eleven_labs, deepgram).`;
  if (provider.type === 'eleven_labs' && (!provider.model_id || !provider.voice_id)) return 'agent.speak.provider needs model_id and voice_id for eleven_labs.';
  return null;
}

function agentSession(ws, requestId, fault) {
  const started = Date.now();
  let settings = null;
  const queue = [];
  let speaking = false;
  let messages = 0;
  let characters = 0;
  let audioBytes = 0;
  let keepAlives = 0;
  let inputBytes = 0;
  let outcome = 'closed';
  let idle = null;
  const touch = () => {
    clearTimeout(idle);
    idle = setTimeout(() => {
      outcome = 'idle_timeout';
      ws.close(1000, 'idle');
    }, IDLE_CLOSE_MS);
  };
  const fail = (code, description) => {
    outcome = `error:${code}`;
    ws.sendText({ type: 'Error', description, code });
  };
  touch();
  if (fault?.mode !== 'no_welcome') ws.sendText({ type: 'Welcome', request_id: requestId });

  async function pump() {
    if (speaking) return;
    speaking = true;
    while (queue.length && ws.open) {
      const message = queue.shift();
      const provider = settings.agent.speak.provider;
      const voiceId = provider.voice_id ?? provider.model ?? 'deepgram';
      const speakFault = takeFault('agent-speak') ?? (fault?.code && fault.code !== 'INVALID_SETTINGS' ? fault : null);
      if (speakFault) {
        fail(speakFault.code || 'FAILED_TO_SPEAK', speakFault.message || 'A fault set with POST /_faults.');
        queue.length = 0;
        break;
      }
      if (provider.type === 'eleven_labs' && !VOICE_IDS.has(provider.voice_id)) {
        fail('FAILED_TO_SPEAK', `ElevenLabs could not speak with voice_id ${provider.voice_id}: the voice was not found.`);
        queue.length = 0;
        break;
      }
      if (provider.type === 'eleven_labs' && !ELEVEN_MODELS.has(provider.model_id)) {
        fail('FAILED_TO_SPEAK', `ElevenLabs has no model ${provider.model_id}.`);
        queue.length = 0;
        break;
      }
      if (!spokenText(message)) {
        fail('FAILED_TO_SPEAK', 'ElevenLabs had no words to speak: the script is only cues (give the cue words, e.g. "[laughs] Ha!").');
        queue.length = 0;
        break;
      }
      messages += 1;
      characters += [...spokenText(message)].length;
      ws.sendText({ type: 'ConversationText', role: 'assistant', content: message });
      await sleep(FIRST_AUDIO_MS);
      if (!ws.open) break;
      ws.sendText({ type: 'AgentStartedSpeaking', total_latency: FIRST_AUDIO_MS / 1000, tts_latency: FIRST_AUDIO_MS / 1000, ttt_latency: 0 });
      const rate = settings.audio?.output?.sample_rate ?? 24000;
      const pcm = synthesize(message, voiceId, rate);
      const chunk = Math.round(rate * 0.2) * 2;
      for (let offset = 0; offset < pcm.length && ws.open; offset += chunk) {
        await ws.sendBinary(pcm.subarray(offset, Math.min(pcm.length, offset + chunk)));
        audioBytes += Math.min(chunk, pcm.length - offset);
        if (fault?.mode === 'close_mid_audio') {
          outcome = 'closed_mid_audio';
          ws.socket.destroy();
          return;
        }
        if (CHUNK_INTERVAL_MS > 0) await sleep(CHUNK_INTERVAL_MS);
      }
      if (ws.open) ws.sendText({ type: 'AgentAudioDone' });
      outcome = 'spoken';
    }
    speaking = false;
  }

  ws.on('text', (text) => {
    touch();
    let message;
    try {
      message = JSON.parse(text);
    } catch {
      return fail('UNPARSABLE_CLIENT_MESSAGE', 'The message was not JSON.');
    }
    switch (message?.type) {
      case 'Settings': {
        const problem = validateSettings(message) ?? (fault?.code === 'INVALID_SETTINGS' ? fault.message || 'A fault set with POST /_faults.' : null);
        if (problem) return fail('INVALID_SETTINGS', problem);
        if (fault?.mode === 'hang') return undefined;
        settings = message;
        return ws.sendText({ type: 'SettingsApplied' });
      }
      case 'InjectAgentMessage': {
        const content = typeof message.message === 'string' ? message.message : message.content;
        if (!settings) return fail('SETTINGS_REQUIRED', 'Send Settings (and wait for SettingsApplied) before InjectAgentMessage.');
        if (typeof content !== 'string' || !content.trim()) return ws.sendText({ type: 'InjectionRefused', message: 'There was nothing to say.' });
        queue.push(content);
        return void pump();
      }
      case 'KeepAlive':
        keepAlives += 1;
        return undefined;
      case 'UpdateSpeak':
      case 'UpdatePrompt':
      case 'InjectUserMessage':
      case 'FunctionCallResponse':
        return undefined;
      default:
        return ws.sendText({ type: 'Warning', description: `The stand-in ignores client messages of type ${JSON.stringify(message?.type)}.`, code: 'UNKNOWN_MESSAGE_TYPE' });
    }
  });
  ws.on('binary', (buffer) => {
    touch();
    inputBytes += buffer.length;
  });
  ws.on('close', (code) => {
    clearTimeout(idle);
    const provider = settings?.agent?.speak?.provider ?? {};
    record({ kind: 'agent', path: '/v1/agent/converse', request_id: requestId, outcome, close_code: code, voice_id: provider.voice_id ?? null, model_id: provider.model_id ?? null,
      sample_rate: settings?.audio?.output?.sample_rate ?? null, messages, characters, audio_bytes: audioBytes, input_bytes: inputBytes, keep_alives: keepAlives, ms: Date.now() - started });
  });
}

// ------------------------------------------------------------------------------------------------ the server

async function controls(req, res, url) {
  if (url.pathname === '/_health') return sendJson(res, 200, { ok: true, service: 'deepgram-stand-in', port: PORT, voices: [...VOICE_IDS] });
  if (url.pathname === '/_requests') {
    if (req.method === 'DELETE') {
      recent.length = 0;
      return sendJson(res, 200, { cleared: true });
    }
    return sendJson(res, 200, { count: recent.length, items: recent });
  }
  if (url.pathname === '/_faults') {
    if (req.method === 'GET') return sendJson(res, 200, { faults });
    if (req.method === 'DELETE') {
      faults = [];
      return sendJson(res, 200, { faults });
    }
    if (req.method === 'POST') {
      let body = '';
      for await (const chunk of req) body += chunk;
      let fault;
      try {
        fault = JSON.parse(body || '{}');
      } catch {
        return sendJson(res, 400, { error: { code: 'invalid_json', message: 'Send a JSON fault, e.g. {"target":"listen","status":503}.' } });
      }
      if (!['listen', 'agent', 'agent-speak'].includes(fault.target)) return sendJson(res, 422, { error: { code: 'invalid_target', message: 'target must be listen, agent or agent-speak.' } });
      const entry = { target: fault.target, status: fault.status ?? null, code: fault.code ?? null, mode: fault.mode ?? null, message: fault.message ?? null, delay_ms: fault.delay_ms ?? 0, remaining: Math.max(1, Number(fault.count ?? 1)) };
      faults.push(entry);
      return sendJson(res, 201, { fault: entry, faults });
    }
  }
  return sendJson(res, 404, { error: { code: 'not_found', message: `No stand-in control at ${url.pathname}.` } });
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  try {
    if (url.pathname.startsWith('/_')) return await controls(req, res, url);
    if (url.pathname === '/v1/listen' && req.method === 'POST') return await listen(req, res, url);
    if (url.pathname === '/v1/agent/converse') {
      res.writeHead(426, { 'content-type': 'text/plain', upgrade: 'websocket' });
      return res.end('The Voice Agent speaks WebSocket: connect with ws://127.0.0.1:4540/v1/agent/converse.\n');
    }
    record({ kind: 'http', method: req.method, path: url.pathname, status: 404 });
    req.resume();
    return deepgramError(res, 404, 'NOT_FOUND', `The stand-in implements POST /v1/listen and ws /v1/agent/converse, not ${req.method} ${url.pathname}.`);
  } catch (error) {
    if (!res.headersSent) deepgramError(res, 500, 'INTERNAL_SERVER_ERROR', `The stand-in failed: ${error.message}`);
    return undefined;
  }
});

server.on('upgrade', (req, socket, head) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const requestId = randomUUID();
  if (url.pathname !== '/v1/agent/converse') {
    record({ kind: 'agent', path: url.pathname, status: 404 });
    return refuseUpgrade(socket, 404, 'Not Found', { err_code: 'NOT_FOUND', err_msg: `No WebSocket at ${url.pathname}; the Voice Agent is /v1/agent/converse.`, request_id: requestId });
  }
  const offered = String(req.headers['sec-websocket-protocol'] || '').split(',').map((value) => value.trim()).filter(Boolean);
  const byProtocol = offered.length >= 2 && ['token', 'bearer'].includes(offered[0].toLowerCase());
  if (!authorized(req.headers) && !byProtocol) {
    record({ kind: 'agent', path: url.pathname, status: 401 });
    return refuseUpgrade(socket, 401, 'Unauthorized', { err_code: 'INVALID_AUTH', err_msg: 'Invalid credentials.', request_id: requestId });
  }
  const fault = takeFault('agent');
  if (fault?.mode === 'reject') {
    record({ kind: 'agent', path: url.pathname, status: fault.status ?? 503, fault: true });
    return refuseUpgrade(socket, fault.status ?? 503, 'Service Unavailable', { err_code: 'SERVICE_UNAVAILABLE', err_msg: 'A fault set with POST /_faults.', request_id: requestId });
  }
  const ws = acceptWebSocket(req, socket, head, { protocol: (list) => list[0] });
  if (ws) agentSession(ws, requestId, fault);
  return undefined;
});

server.listen(PORT, '127.0.0.1', () => {
  process.stdout.write(`deepgram stand-in listening on http://127.0.0.1:${PORT} (log ${LOG})\n`);
});
for (const signal of ['SIGTERM', 'SIGINT']) {
  process.on(signal, () => {
    server.close();
    setTimeout(() => process.exit(0), 300).unref();
  });
}
