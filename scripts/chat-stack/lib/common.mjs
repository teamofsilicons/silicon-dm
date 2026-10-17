// Shared pieces of the local chat stack (scripts/chat-stack): where everything lives, its ports, private files,
// HTTP calls with timeouts, pid files and the one lock that keeps two `up` runs from starting things twice.
//
// Everything the stack writes lives in <worktree>/.dev (git-ignored): secrets in 0600 files, never in a log line or
// on stdout unless a command was asked for exactly that secret (identities.mjs token/slt/cookie).
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync, chmodSync, openSync, closeSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** scripts/chat-stack (this file is in its lib/). */
export const HERE = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const ROOT = resolve(HERE, '..', '..');
export const DEV = join(ROOT, '.dev');

/** Ports of the chat stack (spec §9). The Accounts block is 9839-9846; DM's own block is 4520-4542. */
export const PORTS = Object.freeze({
  postgres: 5500,
  accounts_api: 9839,
  accounts_site: 9840,
  mock_oidc: 9841,
  mock_messaging: 9842,
  fake_apps: 9843,
  mock_iris: 9844,
  accounts_developer: 9845,
  apps_stand_in: 9846,
  web_dev: 4520,
  dm_api: 4521,
  ting_stand_in: 4522,
  web_e2e: 4530,
  deepgram: 4540,
  briefcase: 4541,
  relay: 4542,
  giphy: 4543,
});

export const PATHS = Object.freeze({
  dev: DEV,
  pg: join(DEV, 'pg'),
  pgLog: join(DEV, 'pg.log'),
  bin: join(DEV, 'bin'),
  logs: join(DEV, 'logs'),
  pids: join(DEV, 'pids'),
  locks: join(DEV, 'locks'),
  homes: join(DEV, 'homes'),
  relay: join(DEV, 'relay'),
  briefcase: join(DEV, 'briefcase'),
  dmDir: join(DEV, 'dm'),
  accountsDir: join(DEV, 'accounts'),
  accountsApps: join(DEV, 'accounts', 'apps.json'),
  stack: join(DEV, 'stack.json'),
  secrets: join(DEV, 'secrets.json'),
  identities: join(DEV, 'identities.json'),
  state: join(DEV, 'state.json'),
  dmApiEnv: join(DEV, 'dm-api.env'),
  cache: join(DEV, 'cache'),
  build: join(DEV, 'bin', 'BUILD.json'),
  webEnv: join(ROOT, 'web', '.env.local'),
});

export const LOOPBACK = new Set(['localhost', '127.0.0.1', '::1', '[::1]']);
export const PG_BIN = process.env.PG_BIN || '/opt/homebrew/opt/postgresql@16/bin';
export const ACCOUNTS_CHECKOUT = process.env.SILICON_ACCOUNTS_DIR || resolve(ROOT, '..', '..', 'silicon-accounts');
export const WAVEFORM_ENV = process.env.WAVEFORM_ENV_FILE || resolve(ROOT, '..', '..', 'silicon-waveform', '.env');
export const SHARED_TARGET = '/Users/codanium/Documents/silicon/.worktrees/dm-ting-delivery-20261017/target';
export const GIT = existsSync('/Library/Developer/CommandLineTools/usr/bin/git') ? '/Library/Developer/CommandLineTools/usr/bin/git' : 'git';

/** A failure with the words to show (what happened, why, what to do). */
export class StackError extends Error {
  constructor(message, hint) {
    super(message);
    this.name = 'StackError';
    this.hint = hint ?? null;
  }
}

export function ensureDir(path, mode = 0o700) {
  mkdirSync(path, { recursive: true, mode });
  return path;
}

/** Writes a file only its owner can read, atomically (temporary file + rename). */
export function writePrivate(path, text) {
  ensureDir(dirname(path));
  const temporary = `${path}.${process.pid}.${randomBytes(4).toString('hex')}.tmp`;
  writeFileSync(temporary, text, { mode: 0o600 });
  chmodSync(temporary, 0o600);
  renameSync(temporary, path);
}

export function writeJsonPrivate(path, value) {
  writePrivate(path, `${JSON.stringify(value, null, 2)}\n`);
}

export function readJson(path, fallback = undefined) {
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    if (fallback !== undefined) return fallback;
    if (error.code === 'ENOENT') throw new StackError(`${path} does not exist.`, 'Start the stack first: scripts/chat-stack/up.sh');
    throw new StackError(`${path} is not readable JSON: ${error.message}`);
  }
}

export function stackFile() {
  return readJson(PATHS.stack, null) ?? (() => {
    throw new StackError(`${PATHS.stack} does not exist yet.`, 'Start the stack first: scripts/chat-stack/up.sh');
  })();
}

export const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

/**
 * One HTTP request with a timeout. Returns {status, ok, headers, text, json}; network failures come back as status 0
 * with `error` set, so callers decide whether that is fatal. Never logs headers or bodies.
 */
export async function http(method, url, { headers = {}, json, form, body, timeoutMs = 15_000, basic, bearer } = {}) {
  const options = { method, headers: { accept: 'application/json', ...headers }, redirect: 'manual' };
  if (json !== undefined) {
    options.body = JSON.stringify(json);
    options.headers['content-type'] = 'application/json';
  } else if (form !== undefined) {
    options.body = new URLSearchParams(form).toString();
    options.headers['content-type'] = 'application/x-www-form-urlencoded';
  } else if (body !== undefined) {
    options.body = body;
  }
  if (basic) options.headers.authorization = `Basic ${Buffer.from(`${basic[0]}:${basic[1]}`).toString('base64')}`;
  if (bearer) options.headers.authorization = `Bearer ${bearer}`;
  options.signal = AbortSignal.timeout(timeoutMs);
  try {
    const response = await fetch(url, options);
    const text = await response.text();
    let parsed = null;
    try {
      parsed = text ? JSON.parse(text) : null;
    } catch {
      parsed = null;
    }
    return { status: response.status, ok: response.ok, headers: response.headers, text, json: parsed };
  } catch (error) {
    return { status: 0, ok: false, headers: new Headers(), text: '', json: null, error: error.cause?.code || error.name || String(error) };
  }
}

/** A short description of a failed answer for an error message (an API error's code and message, never a token). */
export function describe(answer) {
  if (answer.status === 0) return `no answer (${answer.error})`;
  const error = answer.json?.error;
  if (error && typeof error === 'object') return `HTTP ${answer.status} ${error.code ?? ''}: ${error.message ?? ''}${error.hint ? ` (hint: ${error.hint})` : ''}`.trim();
  if (typeof error === 'string') return `HTTP ${answer.status} ${error}${answer.json.error_description ? `: ${answer.json.error_description}` : ''}`;
  return `HTTP ${answer.status} ${answer.text.slice(0, 300)}`;
}

/** Is something accepting TCP connections on 127.0.0.1:port? */
export async function portOpen(port, host = '127.0.0.1') {
  const net = await import('node:net');
  return new Promise((done) => {
    const socket = net.createConnection({ port, host });
    const finish = (value) => {
      socket.destroy();
      done(value);
    };
    socket.setTimeout(800, () => finish(false));
    socket.once('connect', () => finish(true));
    socket.once('error', () => finish(false));
  });
}

/** Who listens on a port (for error messages), via lsof. */
export function listener(port) {
  const done = spawnSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN'], { encoding: 'utf8' });
  const line = (done.stdout || '').split('\n')[1];
  if (!line) return null;
  const [command, pid] = line.trim().split(/\s+/);
  return { command, pid: Number(pid) };
}

// ------------------------------------------------------------------------------------------------ pid files

export function pidFile(name) {
  return join(PATHS.pids, `${name}.pid`);
}

/** The pid recorded for `name` when that process is alive and its command still contains `needle`. */
export function livePid(name, needle) {
  let pid;
  try {
    pid = Number(readFileSync(pidFile(name), 'utf8').trim());
  } catch {
    return null;
  }
  if (!Number.isInteger(pid) || pid <= 0) return null;
  try {
    process.kill(pid, 0);
  } catch {
    return null;
  }
  const command = spawnSync('ps', ['-o', 'command=', '-p', String(pid)], { encoding: 'utf8' }).stdout || '';
  return command.includes(needle) ? pid : null;
}

/** Stops the recorded process (its whole process group), waiting up to `graceMs`; true when something was stopped. */
export async function stopRecorded(name, needle, graceMs = 8_000) {
  const pid = livePid(name, needle);
  rmSync(pidFile(name), { force: true });
  if (!pid) return false;
  const signal = (sig) => {
    try {
      process.kill(-pid, sig);
    } catch {
      try {
        process.kill(pid, sig);
      } catch {
        // already gone
      }
    }
  };
  signal('SIGTERM');
  const deadline = Date.now() + graceMs;
  while (Date.now() < deadline) {
    try {
      process.kill(pid, 0);
    } catch {
      return true;
    }
    await sleep(100);
  }
  signal('SIGKILL');
  return true;
}

/** Starts a background process in its own session (so the whole group can be stopped), output appended to `log`. */
export async function startDetached(name, command, args, { env, cwd = ROOT, log }) {
  const { spawn } = await import('node:child_process');
  ensureDir(PATHS.pids);
  ensureDir(dirname(log));
  const fd = openSync(log, 'a', 0o600);
  writeFileSync(fd, `\n==== ${name} started ${new Date().toISOString()} ====\n`);
  const child = spawn(command, args, { env, cwd, detached: true, stdio: ['ignore', fd, fd] });
  closeSync(fd);
  child.unref();
  writePrivate(pidFile(name), String(child.pid));
  return child.pid;
}

// ------------------------------------------------------------------------------------------------ lock

/** Runs `fn` holding .dev/locks/<name> (mkdir is atomic). A lock whose holder died is taken over. */
export async function withLock(name, fn, { waitMs = 15 * 60_000, onWait } = {}) {
  ensureDir(PATHS.locks);
  const dir = join(PATHS.locks, name);
  const started = Date.now();
  let told = false;
  for (;;) {
    try {
      mkdirSync(dir);
      writeFileSync(join(dir, 'pid'), String(process.pid));
      break;
    } catch {
      let owner = null;
      try {
        owner = Number(readFileSync(join(dir, 'pid'), 'utf8'));
      } catch {
        // being written right now, or a crash between mkdir and write
        try {
          if (Date.now() - statSync(dir).mtimeMs > 10_000) owner = -1;
        } catch {
          continue;
        }
      }
      let alive = false;
      if (owner > 0) {
        try {
          process.kill(owner, 0);
          alive = true;
        } catch {
          alive = false;
        }
      }
      if (!alive && owner !== null) {
        rmSync(dir, { recursive: true, force: true });
        continue;
      }
      if (!told && onWait) {
        onWait(owner);
        told = true;
      }
      if (Date.now() - started > waitMs) throw new StackError(`Timed out after ${Math.round(waitMs / 1000)} s waiting for ${dir} (held by pid ${owner}).`, 'Another scripts/chat-stack command is running; wait for it, or remove the lock directory if that process is gone.');
      await sleep(500);
    }
  }
  try {
    return await fn();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** Free space on the disk that holds the worktree, in GiB. */
export function freeGiB() {
  const done = spawnSync('df', ['-k', ROOT], { encoding: 'utf8' });
  const line = (done.stdout || '').trim().split('\n')[1];
  if (!line) return null;
  const available = Number(line.trim().split(/\s+/)[3]);
  return Number.isFinite(available) ? available / 1024 / 1024 : null;
}

/** Reads KEY=VALUE lines (dotenv style, `export ` and quotes allowed); never logs values. */
export function parseDotenv(text) {
  const out = {};
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const match = /^(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/.exec(line);
    if (!match) continue;
    let value = match[2].trim();
    if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) value = value.slice(1, -1);
    out[match[1]] = value;
  }
  return out;
}

export const randomSecret = (bytes = 48) => randomBytes(bytes).toString('base64url');
