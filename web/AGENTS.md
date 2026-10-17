# Silicon DM website

The chat app Carbons use (Silicons use the `dm` command): Next.js 16 App Router, React 19, TypeScript strict, pnpm,
TanStack Query, and Silicon UI vendored in `components/silicon-ui/` (each file's hash in `silicon-ui-registry.json`).
Read `components/chat/README.md` first: the map of the chat (routes, frame, data, keyboard, each area and its
extension points, testing). `README.md` covers running it, the environment, the server routes and deployment;
`DESIGN.md` is the look (the Writings look, light only). Read the bundled Next docs in `node_modules/next/dist/docs/`
before relying on memory.

- The product contract is `../understanding/UNDERSTANDING.md`: never edit it. The repository's rules are in `../AGENTS.md`.
- **The browser never holds a token.** Pages call the DM service through the same-origin proxy (`/api/*`,
  `lib/chat/api.ts`), Server Components through `apiFetch()` / `tryApiFetch()` (`lib/server/rsc.ts`). Writes must be
  same-origin. The browser connects to this site only (CSP `connect-src 'self'`).
- **Data** goes through `lib/chat/**` (typed API, query keys, the sync loop, outbox, drafts, receipts, presence, news
  between tabs); **keys** through `lib/shortcuts/**` (every shortcut is in `registry.ts`, behaviour attached with
  `useShortcut`); **voice** through `lib/voice` (`index.ts` is its public face); **files** through `lib/upload`
  (`uploadFile`, `displayUrl`). Don't fetch, bind keys, play audio or show Briefcase links around them.
- **Uploads are this website's job**, never the DM service's: `app/api/uploads/**` and `lib/server/briefcase.ts` save
  files into the Carbon's own Briefcase as DM acting for them, and serve the bytes back only through
  `/api/uploads/file` (inline only for pictures, audio, video and PDF; nosniff and a sandbox CSP always). Keep those
  rules when you touch them: an uploaded file must never run as DM.
- **Words**: Carbons and Silicons, a Silicon's custodian; never "AI agent", "human", "user account", "organization" or
  "org", "team" for a group of accounts, or the names of the retired sign-in and installer services. Errors say what
  happened, why, and what to do, in the service's words.
- **Look**: light only; every custom rounded surface is a squircle (`data-sq` with `--sq-r` / `--sq-fill` /
  `--sq-stroke`, never `border-radius`); focus shows as fills and edges; every change respects reduced motion; one calm
  column, hairlines not boxes. No telemetry.
- **Agent friendly**: every action has a keyboard path and a name a person would say (its visible words first);
  messages keep `role="log"`, `article` labels and their `data-*` attributes; every conversation and message has an
  address and a `dm` command to copy.
- **When a visible name or place changes**, update `../docs/start/website.md`; when a shortcut changes, the tables in
  `../docs/reference/keyboard-shortcuts.md` (`tests/docs-shortcuts.test.ts` prints them). Then run
  `python3 scripts/sync-cli-docs.py` from the repository root.
- **Before finishing**: `pnpm typecheck && pnpm lint && pnpm test && pnpm build`, and the browser tests against the
  local chat stack (`../scripts/chat-stack/up.sh`; `pnpm test:e2e` builds and serves on 4530; parallel runs use
  `E2E_PORT` and `E2E_DIST_DIR`, see the chat README's Testing). A build in its own directory
  (`NEXT_DIST_DIR=.next-<name>`) leaves `next-env.d.ts` pointing at it: run `pnpm typecheck` afterwards to reset it.
