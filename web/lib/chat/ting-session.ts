/** A Ting 0.4 receiver belongs to this connection, never to a saved DM sign-in.
 * Cookies stay with Ting. References only wake DM's authorized sync reader; no
 * notification content is trusted and no Ting ACK or read receipt is sent. */
export interface TingAccount { uuid?: string; type: string }
export interface TingConnection {
  state: "connecting" | "connected" | "sign-in" | "unavailable";
  sessionId: string | null;
}
export interface TingSessionOptions {
  origin: string;
  account: TingAccount;
  appId: string;
  changed(): void;
  status(value: TingConnection): void;
}

const text = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 255 && !/[\s\x00-\x1f\x7f]/.test(value);
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);

/** Uses the exact origin configured by the website operator, including for /me. */
export function openTingSession(options: TingSessionOptions): { close(): void; reconnect(): void } {
  const base = new URL(options.origin);
  if (base.origin !== options.origin || (base.protocol !== "https:" && !(base.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(base.hostname)))) throw new Error("Invalid Ting origin");
  let stopped = false, epoch = 0, failures = 0;
  let socket: WebSocket | undefined;
  let abort: AbortController | undefined;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let deadline: ReturnType<typeof setTimeout> | undefined;
  let healthy: ReturnType<typeof setTimeout> | undefined;
  const report = (state: TingConnection["state"], sessionId: string | null = null) => options.status({ state, sessionId });
  const clear = () => {
    clearTimeout(deadline); clearTimeout(healthy);
    abort?.abort();
    socket?.close(1000, "receiver-ended");
    socket = undefined;
  };
  const schedule = () => {
    if (stopped || retry) return;
    const delay = Math.min(30_000, 1000 * 2 ** Math.min(failures++, 5) * (0.8 + Math.random() * 0.4));
    retry = setTimeout(() => { retry = undefined; void connect(); }, delay);
  };
  const end = (state: TingConnection["state"], again: boolean) => {
    ++epoch;
    clear();
    report(state);
    options.changed(); // catch up even when the last delivery was lost
    if (again) schedule();
  };
  async function connect() {
    const run = ++epoch;
    clear();
    if (stopped) return;
    report("connecting");
    abort = new AbortController();
    deadline = setTimeout(() => { if (run === epoch) end("unavailable", true); }, 10_000);
    try {
      // A handle may change; only the immutable UUID and kind bind this receiver.
      const response = await fetch(new URL("/v1/me", base), { credentials: "include", cache: "no-store", redirect: "error", signal: abort.signal });
      if (stopped || run !== epoch) return;
      if (response.status === 401) { end("sign-in", false); return; }
      if (!response.ok) throw new Error("Ting unavailable");
      const me: unknown = await response.json();
      if (stopped || run !== epoch) return;
      if (!object(me) || me.authenticated !== true || !options.account.uuid || me.uuid !== options.account.uuid || me.kind !== options.account.type) { end("sign-in", false); return; }
      const url = new URL("/v1/ws?protocol=v1", base);
      url.protocol = base.protocol === "https:" ? "wss:" : "ws:";
      const target = new WebSocket(url);
      socket = target;
      let sessionId: string | null = null;
      let phase: "ready" | "register" | "watch" | "active" = "ready";
      const registerId = crypto.randomUUID(), watchId = crypto.randomUUID();
      const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
      target.onmessage = event => {
        if (stopped || run !== epoch) return;
        let frame: unknown;
        try {
          if (typeof event.data !== "string" || event.data.length > 1024 * 1024) throw new Error("Invalid frame");
          frame = JSON.parse(event.data);
          if (!object(frame)) throw new Error("Invalid frame");
        } catch { end("unavailable", true); return; }
        if (!object(frame)) return;
        if (frame.op === "ready" && phase === "ready") {
          if (frame.protocol !== "v1" || !text(frame.session_id)) { end("unavailable", true); return; }
          sessionId = frame.session_id;
          phase = "register";
          target.send(JSON.stringify({ op: "register", request_id: registerId, timezone }));
        } else if (frame.op === "registered" && phase === "register" && frame.request_id === registerId) {
          // /me and the upgrade can race a Ting account switch. Prove which account
          // was attached to THIS socket before using any of its events.
          if (frame.session_id !== sessionId || frame.for !== options.account.uuid) { end("sign-in", false); return; }
          phase = "watch";
          target.send(JSON.stringify({ op: "watch_inbox", request_id: watchId, timezone }));
        } else if (frame.op === "watching_inbox" && phase === "watch" && frame.request_id === watchId) {
          if (frame.session_id !== sessionId) { end("unavailable", true); return; }
          phase = "active";
          clearTimeout(deadline);
          healthy = setTimeout(() => { failures = 0; }, 60_000);
          report("connected", sessionId);
          options.changed();
        } else if (frame.op === "paused" || frame.op === "error") {
          const code = frame.reason ?? (object(frame.error) ? frame.error.code : null);
          const denied = ["session_expired", "authentication_required", "permission_changed", "forbidden"].includes(String(code));
          end(denied ? "sign-in" : "unavailable", !denied);
        } else if (phase === "active" && frame.op === "inbox_changed") {
          options.changed();
        } else if (phase === "active" && (frame.op === "tings" || frame.op === "silent_digest") && frame.session_id === sessionId && frame.webhook_id == null && Array.isArray(frame.tings)) {
          if (frame.tings.some(item => object(item) && item.for === options.account.uuid && (item.session_id == null || item.session_id === sessionId) && (item.type === `${options.appId}.syncChanged` || item.type === `${options.appId}.sync.changed`))) options.changed();
        }
      };
      target.onclose = () => { if (!stopped && run === epoch) end("unavailable", true); };
      // Browsers deliver onclose after an error. The handshake deadline also
      // covers transports that never open; protocol ping/pong is browser-owned.
      target.onerror = () => { if (!stopped && run === epoch) end("unavailable", true); };
    } catch { if (!stopped && run === epoch) end("unavailable", true); }
  }
  const reconnect = () => {
    if (stopped) return;
    clearTimeout(retry); retry = undefined;
    void connect();
  };
  const resume = () => { if (document.visibilityState === "visible") reconnect(); };
  window.addEventListener("online", reconnect);
  window.addEventListener("focus", resume);
  document.addEventListener("visibilitychange", resume);
  void connect();
  return {
    reconnect,
    close() {
      stopped = true; ++epoch;
      clearTimeout(retry); clear();
      window.removeEventListener("online", reconnect);
      window.removeEventListener("focus", resume);
      document.removeEventListener("visibilitychange", resume);
    },
  };
}
