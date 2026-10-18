# Silicon DM docs site

Builds [docs.dm.teamofsilicons.com](https://docs.dm.teamofsilicons.com) from `../docs`. No credentials are needed.

```sh
npm ci            # markdown-it, pinned in package-lock.json (npm ci --offline works from the npm cache)
npm run build     # dist/: one page per docs/**/*.md, plus the installer, the OpenAPI document and llms files
npm run check     # canonical URLs, descriptions, local links, anchors, navigation, llms.txt
```

How a page is made:

- Every Markdown file under `docs/` is a page, except `docs/history` and `docs/migration` (records, not docs).
  `docs/index.md` is `/`; `docs/start/voice.md` is `/start/voice/`, and its Markdown is served at `/start/voice.md`.
- Every page opens with front matter: `title`, `description`, `kind` (`instructive`, `informative` or `reference`),
  `order` and `related` (paths under `docs/`). The build refuses a page without it, and
  `scripts/sync-cli-docs.py --check` checks the same rules. Quote a value that contains `: `.
- The navigation has one group per folder, in this order: Start, Learn, Reference, Operations; pages are sorted by
  `order`, then title. A new folder needs a section in `build.mjs` first.
- `related` is shown after the article, unless the page has its own `## Related` section.
- Links between pages are written as relative `.md` paths (they work on GitHub, in `dm docs` and here); links to
  other repository files become GitHub links. Heading ids are made as GitHub makes them, so `page.md#heading` lands on
  the same heading everywhere; `npm run check` fails on a link to a heading that does not exist.
- Rust examples are doctests: lines starting with `# ` (rustdoc's hidden lines) are left out of the page, as rustdoc
  does, and stay in the Markdown copy.
- On phones the article comes first, with an "All pages" link to the list of pages after it, and every table folds
  into rows whose values name their column (the build puts the column's name on each cell, and table roles on the
  table, so screen readers keep the table).
- Code blocks and tables take the keyboard's focus, so one wider than the page can be scrolled without a mouse; the
  skip link moves focus to the article.
- `llms.txt` lists every page with its description; `llms-full.txt` is every page's text.

When a page moves, add its old address to the `redirects` in `vercel.json`, so links from older releases (and from the
DM service's `GET /api/v1/contracts`) keep working.

Publish `dist` to the dedicated `silicon-dm-docs` Vercel project at `docs.dm.teamofsilicons.com`
(`docs/operations/releasing.md`). Build with `DOCS_RELEASE_PREVIEW=1` to show a banner saying the release is not live
yet. The site has no scripts: its Content-Security-Policy allows none.
