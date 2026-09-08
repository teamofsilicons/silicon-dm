import {
  createHmac,
  randomBytes,
  randomUUID,
  timingSafeEqual,
} from "node:crypto";
import { constants } from "node:fs";
import {
  chmod,
  lstat,
  mkdir,
  open,
  readFile,
  readdir,
  rename,
  unlink,
} from "node:fs/promises";
import { join } from "node:path";
import type { IncomingMessage } from "node:http";
import type { Config } from "./config.ts";

export type Actor = { id: string; type: "carbon" | "silicon" };
export type Profile = {
  profile_id: string;
  actor: Actor;
  organization_id: string;
  testing_environment_id?: string;
  testing_key?: string;
  access_token: string;
  refresh_token: string;
  expires_at: number;
  auth_required?: boolean;
};
export type BrowserSession = {
  version: 1;
  binding: string;
  deadline: number;
  selected?: string;
  profiles: Profile[];
  flow?: { state: string; deadline: number };
};
export type Browser = { id: string; value: BrowserSession };
export class GatewayError extends Error {
  status: number;
  code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}
export const uuidPattern =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const opaquePattern = /^[A-Za-z0-9_-]{43}$/;
const maxSessionBytes = 16 * 1024 * 1024;
const lifetime = 30 * 24 * 60 * 60 * 1000;

export function publicProfile(profile: Profile) {
  return {
    profile_id: profile.profile_id,
    actor: profile.actor,
    organization_id: profile.organization_id,
    testing_environment_id: profile.testing_environment_id,
    authenticated: !profile.auth_required,
  };
}
export function selectProfile(browser: Browser, requested?: string): Profile {
  const id = requested || browser.value.selected;
  const profile = browser.value.profiles.find((p) => p.profile_id === id);
  if (!profile)
    throw new GatewayError(
      401,
      "login_required",
      "Sign in to this browser profile.",
    );
  return profile;
}
export function constantEqual(a: string, b: string): boolean {
  const left = Buffer.from(a),
    right = Buffer.from(b);
  return left.length === right.length && timingSafeEqual(left, right);
}

/** One process owns this directory. All auth changes for a browser serialize. */
export class Sessions {
  private flights = new Map<string, Promise<unknown>>();
  private key!: Buffer;
  private lockPath: string;
  private owner = randomUUID();
  constructor(readonly config: Config) {
    this.lockPath = join(config.directory, "gateway.lock");
  }
  async initialize(): Promise<void> {
    await mkdir(this.config.directory, { recursive: true, mode: 0o700 });
    if ((await lstat(this.config.directory)).isSymbolicLink())
      throw new Error("Session directory must not be a symlink.");
    await chmod(this.config.directory, 0o700);
    try {
      const lock = await open(this.lockPath, "wx", 0o600);
      await lock.writeFile(
        JSON.stringify({ pid: process.pid, owner: this.owner }),
      );
      await lock.sync();
      await lock.close();
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
      let previous: { pid: number };
      try {
        previous = JSON.parse(await readFile(this.lockPath, "utf8"));
      } catch {
        throw new Error(
          "Invalid gateway lock; verify no gateway uses this state directory before removing it.",
        );
      }
      if (!Number.isSafeInteger(previous.pid) || previous.pid <= 0)
        throw new Error("Invalid gateway lock.");
      try {
        process.kill(previous.pid, 0);
      } catch (e) {
        if ((e as NodeJS.ErrnoException).code === "ESRCH") {
          await unlink(this.lockPath);
          return this.initialize();
        }
        throw e;
      }
      throw new Error(
        "Another gateway owns DM_WEB_STATE_DIR; use one process per private directory.",
      );
    }
    try {
      const path = join(this.config.directory, "idempotency.key");
      try {
        const file = await open(path, "wx", 0o600);
        await file.writeFile(randomBytes(32));
        await file.sync();
        await file.close();
      } catch (e) {
        if ((e as NodeJS.ErrnoException).code !== "EEXIST") throw e;
      }
      if ((await lstat(path)).isSymbolicLink())
        throw new Error("Session key must not be a symlink.");
      await chmod(path, 0o600);
      this.key = await readFile(path);
      if (this.key.length !== 32)
        throw new Error("Invalid persisted gateway idempotency key.");
      await this.cleanup();
    } catch (error) {
      await this.close();
      throw error;
    }
  }
  async close(): Promise<void> {
    try {
      const lock = JSON.parse(await readFile(this.lockPath, "utf8"));
      if (lock.owner === this.owner) await unlink(this.lockPath);
    } catch {
      /* Do not remove a lock owned by a replacement process. */
    }
  }
  keyFor(purpose: string, secret: string): string {
    return `dm-web-${createHmac("sha256", this.key).update(this.binding()).update("\0").update(purpose).update("\0").update(secret).digest("hex")}`;
  }
  private binding(): string {
    return `${this.config.origin.origin}|${this.config.frontend.origin}|${this.config.api.origin}|${this.config.appId}`;
  }
  cookie(id: string): string {
    return `${this.config.cookie}=${id}; Max-Age=${lifetime / 1000}; ${this.config.cookieOptions}`;
  }
  cookieId(req: IncomingMessage): string | undefined {
    const values = (req.headers.cookie || "")
      .split(";")
      .map((p) => p.trim())
      .filter((p) => p.startsWith(`${this.config.cookie}=`));
    if (values.length !== 1) return;
    const value = values[0]!.slice(this.config.cookie.length + 1);
    return opaquePattern.test(value) ? value : undefined;
  }
  async read(id?: string): Promise<Browser | undefined> {
    if (!id || !opaquePattern.test(id)) return;
    try {
      const path = join(this.config.directory, `${id}.json`);
      const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
      let value: BrowserSession;
      try {
        const stat = await file.stat();
        if (!stat.isFile() || stat.size > maxSessionBytes)
          throw new Error("Invalid session file.");
        value = JSON.parse(await file.readFile("utf8"));
      } finally {
        await file.close();
      }
      if (
        value.version !== 1 ||
        value.binding !== this.binding() ||
        !Array.isArray(value.profiles) ||
        value.deadline <= Date.now()
      )
        return;
      return { id, value };
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === "ENOENT") return;
      throw e;
    }
  }
  async create(): Promise<Browser> {
    const files = await readdir(this.config.directory);
    if (
      files.filter((f) => /^[A-Za-z0-9_-]{43}\.json$/.test(f)).length >= 10000
    ) {
      throw new GatewayError(
        503,
        "session_capacity",
        "The gateway cannot start another browser session right now.",
      );
    }
    return {
      id: randomBytes(32).toString("base64url"),
      value: {
        version: 1,
        binding: this.binding(),
        deadline: Date.now() + lifetime,
        profiles: [],
      },
    };
  }
  async save(browser: Browser): Promise<void> {
    const serialized = JSON.stringify(browser.value);
    if (Buffer.byteLength(serialized) > maxSessionBytes)
      throw new GatewayError(
        409,
        "profile_limit",
        "Sign out of another account before adding this login.",
      );
    const temporary = join(
      this.config.directory,
      `${browser.id}.${randomUUID()}.tmp`,
    );
    const file = await open(temporary, "wx", 0o600);
    try {
      await file.writeFile(serialized);
      await file.sync();
    } finally {
      await file.close();
    }
    await rename(temporary, join(this.config.directory, `${browser.id}.json`));
    const directory = await open(this.config.directory, "r");
    try {
      await directory.sync();
    } finally {
      await directory.close();
    }
  }
  async locked<T>(id: string, work: () => Promise<T>): Promise<T> {
    const previous = this.flights.get(id) || Promise.resolve();
    const flight = previous.catch(() => {}).then(work);
    this.flights.set(id, flight);
    try {
      return await flight;
    } finally {
      if (this.flights.get(id) === flight) this.flights.delete(id);
    }
  }
  async cleanup(): Promise<void> {
    for (const file of await readdir(this.config.directory)) {
      if (!/^[A-Za-z0-9_-]{43}\.json$/.test(file)) continue;
      try {
        const id = file.slice(0, -5);
        if (!(await this.read(id)))
          await unlink(join(this.config.directory, file));
      } catch {
        /* Corrupt private files remain for operator inspection. */
      }
    }
  }
}
