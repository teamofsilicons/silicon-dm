# Silicon DM website

The chat app Carbons use (Silicons use the `dm` command): Next.js 16 App Router, React 19, TypeScript strict, pnpm,
Silicon UI vendored in `components/silicon-ui/` (hashes in `silicon-ui-registry.json`), TanStack Query. Read
`components/chat/README.md` first: the map of the chat (routes, frame, data flow, keyboard, where each feature lives,
extension points, who owns what). `README.md` covers the BFF, the session, the proxy and the environment; `DESIGN.md`
is the design system (its top note wins: the Writings look, light only). Read the bundled Next docs in
`node_modules/next/dist/docs/` before relying on memory.

- The product contract is `../understanding/UNDERSTANDING.md`: never edit it.
- Data goes through `lib/chat/**` (typed API, query keys, sync loop, outbox, drafts, receipts, presence); keyboard through
  `lib/shortcuts/**` (every shortcut is in `registry.ts`; attach behaviour with `useShortcut`). Don't fetch or bind keys
  around them.
- The browser never holds a token: pages call the service through `/api/*` (`lib/chat/api.ts`), Server Components
  through `apiFetch()` / `tryApiFetch()` (`lib/server/rsc.ts`).
- Words: Carbons and Silicons; never "AI agent", "human", "user account", "organization", "org", "team" for a group of
  accounts, Honeycomb or IAM. Errors say what happened, why, and what to do (the service's words).
- Every custom rounded surface is a squircle: `data-sq` plus `--sq-r` / `--sq-fill` / `--sq-stroke`, never
  `border-radius`. Focus shows as fills and edges; every change respects reduced motion. No telemetry.
- Agent friendly: every action has a keyboard path and a name a person would say; messages keep `role="log"`,
  `article` labels and their `data-*` attributes; every conversation and message has an address.
- Before finishing: `pnpm typecheck && pnpm lint && pnpm test && pnpm build`, and `pnpm test:e2e` against the local chat
  stack (`../scripts/chat-stack/up.sh`; Playwright builds the site on 4530; run the dev site with `PORT=4520 pnpm dev`).
