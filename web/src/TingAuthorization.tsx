import { Show, createSignal } from "solid-js";
import { api } from "./api";
import type { Session } from "./models";

/** Consent codes stay in component memory; the server owns delegated tokens. */
export function TingAuthorization(props: { session: Session; enabled: () => void }) {
  const [request, setRequest] = createSignal<{ authorization_id: string; authorization_url: string }>();
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  let startKey = crypto.randomUUID();
  let finishKey = crypto.randomUUID();
  async function start() {
    setBusy(true); setError("");
    try {
      const value = await api<{ authorization_id: string; authorization_url: string }>("/delivery/authorization", { session: props.session, method: "POST", body: {}, idempotencyKey: startKey });
      const url = new URL(value.authorization_url);
      if (url.protocol !== "https:" && !(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))) throw new Error("The authorization link is invalid.");
      setRequest(value);
    } catch (e) { setError(e instanceof Error ? e.message : "Authorization could not start."); }
    finally { setBusy(false); }
  }
  async function finish(event: SubmitEvent) {
    event.preventDefault(); if (!request()) return;
    setBusy(true); setError("");
    try {
      await api("/delivery/authorization/complete", { session: props.session, method: "POST", body: { authorization_id: request()!.authorization_id, authorization_code: code().trim() }, idempotencyKey: finishKey });
      setCode(""); setRequest(undefined); props.enabled();
    } catch (e) { setError(e instanceof Error ? e.message : "Authorization could not complete."); }
    finally { setBusy(false); }
  }
  return <div class="ting-authorization">
    <Show when={!request()}><button class="text-button" disabled={busy()} onClick={() => void start()}>{busy() ? "Preparing…" : "Authorize Ting delivery"}</button></Show>
    <Show when={request()}>{value => <form onSubmit={finish}>
      <p>Approve notification registration and sends in IAM, keeping this DM account and organization selected.</p>
      <a href={value().authorization_url} target="_blank" rel="noopener noreferrer">Review permission in IAM ↗</a>
      <label>Approval code<input type="password" autocomplete="off" value={code()} onInput={e => setCode(e.currentTarget.value)} required /></label>
      <button type="button" disabled={busy()} onClick={() => { setCode(""); setRequest(undefined); startKey = crypto.randomUUID(); finishKey = crypto.randomUUID(); }}>Cancel</button>
      <button disabled={busy() || !code().trim()}>{busy() ? "Connecting…" : "Complete authorization"}</button>
    </form>}</Show>
    <Show when={error()}><p role="alert">{error()}</p></Show>
  </div>;
}
