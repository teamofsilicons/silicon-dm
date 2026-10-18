// Signs one Carbon in to the account site of the LOCAL Silicon Accounts stack (app `silicon-accounts`) with an email
// code read from the stack's mock sender, creating the Carbon when it is new, and prints its browser session:
//
//   <accounts>/testkit/node_modules/.bin/tsx carbon-signin.mts --accounts-dir DIR --api URL --messaging URL \
//     --email dmc-ada@example.test --id c:dmc-ada --name "Ada Marlow"
//   -> {"uuid","id","display_name","cookies":[{"name","value","expires_ms"}]}
//
// identities.mjs runs this only when a Carbon has no working browser session yet: it costs one email code, and the
// stack allows 10 per address and 30 per network in 10 minutes (each run sends from its own made-up network address,
// X-Forwarded-For, so the budget of the other test suites on this stack is untouched). The session cookie lasts
// 900 days and never rotates, so every later token, short-lived token or Silicon is made from it without a code.
import { pathToFileURL } from 'node:url';

function arg(name: string): string {
  const index = process.argv.indexOf(`--${name}`);
  const value = index >= 0 ? process.argv[index + 1] : undefined;
  if (!value) {
    console.error(`carbon-signin: --${name} is required`);
    process.exit(2);
  }
  return value;
}

const accountsDir = arg('accounts-dir');
const api = arg('api');
const messagingUrl = arg('messaging');
for (const url of [api, messagingUrl]) {
  if (!['localhost', '127.0.0.1', '[::1]'].includes(new URL(url).hostname)) {
    console.error(`carbon-signin: ${url} is not on this machine; this helper only talks to a local stack`);
    process.exit(2);
  }
}

const testkit = await import(pathToFileURL(`${accountsDir}/testkit/lib/index.ts`).href);
const accounts = new testkit.AccountsClient(api, { forwardedFor: 'random' });
const messaging = new testkit.MockMessagingClient(messagingUrl);
const publicUrl: string = await accounts.publicUrl();

const result = await testkit.signInWithCode({
  accounts,
  messaging,
  appId: 'silicon-accounts',
  email: arg('email'),
  redirectUri: `${publicUrl}/`,
  signup: { id: arg('id'), display_name: arg('name'), timezone: 'UTC' },
});
const me = await result.browser.account().me();

// The jar keeps cookies per origin; the account site's session is the one that matters (sa_session).
type Stored = { name: string; value: string; path: string; expiresMs: number | null };
const store = (result.browser.jar as unknown as { store: Map<string, Map<string, Stored>> }).store;
const cookies: Array<{ name: string; value: string; expires_ms: number | null }> = [];
for (const perOrigin of store?.values() ?? []) {
  for (const cookie of perOrigin.values()) {
    if (cookie.name === 'sa_flow' || cookie.name === 'sa_signup') continue;
    if (!cookies.some((kept) => kept.name === cookie.name)) cookies.push({ name: cookie.name, value: cookie.value, expires_ms: cookie.expiresMs });
  }
}
if (!cookies.some((cookie) => cookie.name.endsWith('sa_session'))) {
  console.error(`carbon-signin: the sign-in of ${arg('email')} finished without an account-site session cookie (got: ${cookies.map((c) => c.name).join(', ') || 'none'})`);
  process.exit(1);
}
console.log(JSON.stringify({ uuid: me.uuid, id: me.id, display_name: me.display_name ?? null, cookies }));
