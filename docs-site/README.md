# Silicon DM docs site

Run `npm ci`, `npm run build`, and `npm run check` in this directory. The build
renders canonical Markdown from `../docs`, includes the installer and OpenAPI,
and produces a static site in `dist`. No credentials are needed in the build.

Publish `dist` to the dedicated `silicon-dm-docs` Vercel project and attach
`docs.dm.teamofsilicons.com`. The API and frontend deployments are independent.
