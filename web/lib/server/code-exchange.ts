/** A code is single-use even when two callbacks carry the same still-valid pending cookie. */
import { createHash } from "node:crypto";
import { callbackUrl, serverEnv } from "./env";
import { refreshStore, stateKey } from "./refresh-store";
import { SIGNIN_TTL_MS, type StoredSession } from "./session";
import type { TokenResult } from "./tokens";

interface CodeExchange {
  binding: string;
  started: number;
  result?: TokenResult;
}
const digest = (value: string) => createHash("sha256").update(value).digest("hex");
const interrupted = (): TokenResult => ({ ok: false, status: 400, error: "invalid_grant", description: "This sign-in could not finish. Start sign-in again for a new code." });

export async function exchangeOnce(
  code: string,
  verifier: string,
  exchange: () => Promise<TokenResult>,
  currentSession: (session: StoredSession) => Promise<TokenResult>,
): Promise<TokenResult> {
  const store = await refreshStore();
  const key = stateKey("code", digest(JSON.stringify([serverEnv().accountsApiUrl, callbackUrl(), code])));
  const binding = digest(verifier);
  const started = Date.now();
  // Outlive any pending cookie that could have presented this code. Neither the code nor verifier is stored.
  const expires = started + SIGNIN_TTL_MS + 60_000;
  if (await store.write<CodeExchange>(key, null, { binding, started }, expires)) {
    // Persist the claim BEFORE sending. After a crash or ambiguous response, never try this code again:
    // Accounts may already have consumed it, and replay would revoke the tokens it just issued.
    const result = await exchange();
    if (!await store.write<CodeExchange>(key, 0, { binding, started, result }, expires)) throw new Error("Code exchange result could not be saved.");
    return result;
  }
  for (;;) {
    const record = await store.read<CodeExchange>(key);
    if (!record || record.value.binding !== binding) return interrupted();
    if (record.value.result) {
      const result = record.value.result;
      // Resolve durable state so a late callback cannot restore a signed-out or older session.
      return result.ok ? currentSession(result.session) : result;
    }
    // The token call times out after 20 seconds. A stopped owner is never replaced by a second exchange.
    if (Date.now() - record.value.started >= 25_000) return interrupted();
    await new Promise(resolve => setTimeout(resolve, 50));
  }
}
