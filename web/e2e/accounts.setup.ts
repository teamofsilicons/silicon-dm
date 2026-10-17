/**
 * Once per run: two Carbons signed in to Silicon Accounts, their browser state kept in test-results/.auth/. Specs start
 * from that Accounts sign-in and continue as them ("Continue as …"), a sign-in of their own at DM.
 *
 * Against the local chat stack (scripts/chat-stack/up.sh) the Carbons are the stack's own (ada and bo): their browser
 * state comes from its identity helper, so no email code is spent. Without the stack's helper, two new Carbons sign in
 * through the hosted pages with an email code each, as before.
 */
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test as setup } from "@playwright/test";
import { accountsFile, BASE, sameOrigin, sessionOf, signIn, statePath, uniqueEmail, type Who } from "./support";

const HELPER = join(process.cwd(), "..", "scripts", "chat-stack", "identities.mjs");
const STACK = join(process.cwd(), "..", ".dev", "stack.json");
const POOL: Record<Who, string> = { carbon: "ada", friend: "bo" };

setup("sign two Carbons in once for the whole run", async ({ browser }) => {
  mkdirSync("test-results/.auth", { recursive: true });
  const accounts: Record<string, { email: string; id: string }> = {};
  if (existsSync(HELPER) && existsSync(STACK)) {
    for (const who of ["carbon", "friend"] as Who[]) {
      const answer = JSON.parse(execFileSync("node", [HELPER, "storage-state", POOL[who], "--out", statePath(who)], { encoding: "utf8" })) as { account: { id: string } };
      const identity = JSON.parse(execFileSync("node", [HELPER, "show", POOL[who]], { encoding: "utf8" })) as { email: string };
      accounts[who] = { email: identity.email, id: answer.account.id };
    }
  } else {
    for (const who of ["carbon", "friend"] as Who[]) {
      const context = await browser.newContext({ baseURL: BASE });
      const page = await context.newPage();
      const email = uniqueEmail(who);
      await signIn(page, email);
      accounts[who] = { email, id: (await sessionOf(context)).account!.id };
      await context.storageState({ path: statePath(who) });
      // The kit's own sign-in ends here (revoked); the Silicon Accounts sign-in stays for "Continue as".
      await context.request.post("/auth/sign-out", { headers: sameOrigin() });
      await context.close();
    }
  }
  writeFileSync(accountsFile, `${JSON.stringify(accounts, null, 2)}\n`);
});
