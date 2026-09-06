import { cp, mkdir, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

// Vercel serves files only. DM's authenticated HTTP/WS gateway is a separate
// persistent Node process; no requests or credentials pass through Functions.
const raw = process.env.VITE_DM_GATEWAY_ORIGIN;
if (!raw)
  throw new Error(
    "Set the public VITE_DM_GATEWAY_ORIGIN before building the static Vercel frontend.",
  );
const gateway = new URL(raw);
if (
  gateway.protocol !== "https:" ||
  gateway.pathname !== "/" ||
  gateway.search ||
  gateway.hash ||
  gateway.username ||
  gateway.password
)
  throw new Error(
    "VITE_DM_GATEWAY_ORIGIN must be an exact HTTPS gateway origin.",
  );
const websocket = new URL(gateway);
websocket.protocol = "wss:";
const output = resolve(".vercel/output");
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
await cp("dist/client", `${output}/static`, { recursive: true });
await writeFile(
  `${output}/config.json`,
  JSON.stringify(
    {
      version: 3,
      routes: [
        {
          src: "/(.*)",
          headers: {
            "X-Content-Type-Options": "nosniff",
            "Referrer-Policy": "no-referrer",
            "X-Frame-Options": "DENY",
            "Content-Security-Policy": `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' https: data: blob:; media-src 'self' https: blob:; font-src 'self'; connect-src 'self' ${gateway.origin} ${websocket.origin}; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self' ${gateway.origin}`,
          },
          continue: true,
        },
        {
          src: "/assets/(.*)",
          headers: { "Cache-Control": "public, max-age=31536000, immutable" },
          continue: true,
        },
        {
          src: "/(?:index\\.html)?",
          headers: { "Cache-Control": "no-store" },
          continue: true,
        },
        { handle: "filesystem" },
        {
          src: "/(.*)",
          dest: "/index.html",
          headers: { "Cache-Control": "no-store" },
        },
      ],
    },
    null,
    2,
  ),
);
console.info(
  "Prepared static Vercel output; no Functions, session files, or server credentials are included.",
);
