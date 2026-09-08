import { homedir } from "node:os";
import { isAbsolute, resolve, sep } from "node:path";

export type Config = {
  origin: URL;
  frontend: URL;
  api: URL;
  iam: URL;
  appId: string;
  directory: string;
  maxBytes: number;
  cookie: string;
  cookieOptions: string;
  production: boolean;
};

export function isLoopback(host: string): boolean {
  return ["localhost", "127.0.0.1", "[::1]"].includes(host);
}
function configuredOrigin(value: string): URL {
  const url = new URL(value);
  if (
    url.username ||
    url.password ||
    url.search ||
    url.hash ||
    url.pathname !== "/" ||
    !(
      url.protocol === "https:" ||
      (url.protocol === "http:" && isLoopback(url.hostname))
    )
  ) {
    throw new Error(
      "Gateway origins must be exact HTTPS origins, or HTTP loopback origins.",
    );
  }
  return url;
}
export function configuration(env: NodeJS.ProcessEnv = process.env): Config {
  const production = env.NODE_ENV === "production";
  if (production && !env.DM_WEB_ORIGIN)
    throw new Error("DM_WEB_ORIGIN is required in production.");
  const origin = configuredOrigin(env.DM_WEB_ORIGIN || "http://127.0.0.1:4315");
  const frontend = configuredOrigin(env.DM_FRONTEND_ORIGIN || origin.origin);
  const directory =
    env.DM_WEB_STATE_DIR || resolve(homedir(), ".silicon-dm", "web");
  if (!isAbsolute(directory))
    throw new Error("DM_WEB_STATE_DIR must be absolute.");
  const workingDirectory = resolve(process.cwd());
  if (
    resolve(directory) === workingDirectory ||
    resolve(directory).startsWith(workingDirectory + sep)
  )
    throw new Error(
      "Keep DM_WEB_STATE_DIR outside the application checkout and public assets.",
    );
  const maxBytes = Number(env.DM_WEB_MAX_BODY_BYTES || 128 * 1024 * 1024);
  if (
    !Number.isSafeInteger(maxBytes) ||
    maxBytes < 16384 ||
    maxBytes > 3 * 1024 ** 3
  ) {
    throw new Error(
      "DM_WEB_MAX_BODY_BYTES must be between 16384 and 3221225472.",
    );
  }
  const appId = env.DM_WEB_APP_ID || "tos>dm";
  if (!/^[a-z0-9_-]+>[a-z0-9_-]+$/.test(appId))
    throw new Error("Invalid canonical DM app ID.");
  return {
    origin,
    frontend,
    api: configuredOrigin(
      env.DM_API_ORIGIN || "https://backend.dm.teamofsilicons.com",
    ),
    iam: configuredOrigin(
      env.IAM_LOGIN_ORIGIN || "https://auth.iam.teamofsilicons.com",
    ),
    appId,
    directory: resolve(directory),
    maxBytes,
    production,
    cookie:
      origin.protocol === "https:"
        ? "__Host-dm_browser"
        : `dm_browser_dev_${origin.port || "80"}`,
    cookieOptions: `Path=/; HttpOnly; SameSite=Lax${origin.protocol === "https:" ? "; Secure" : ""}`,
  };
}
