import type { Actor } from "./models";

export type TingState =
  | "connecting"
  | "connected"
  | "unverified"
  | "sign-in"
  | "blocked"
  | "paused";
export interface TingStatus {
  state: TingState;
  message: string;
  origin: string;
}
export interface TingEnvironment {
  testing_environment_id: string | null;
  testing_generation: number | null;
}
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
function verifyEnvironment(value: unknown, expected: TingEnvironment): boolean {
  // Older Ting servers omit this attestation. Missing never means production.
  if (value === undefined) return false;
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("Ting returned an invalid session environment.");
  const environment = value as Record<string, unknown>;
  if (environment.kind === "production") {
    if (
      environment.id !== undefined ||
      environment.generation !== undefined ||
      expected.testing_environment_id !== null ||
      expected.testing_generation !== null
    )
      throw new Error("Ting and DM are using different session environments.");
  } else if (
    environment.kind !== "testing" ||
    typeof environment.id !== "string" ||
    !uuid.test(environment.id) ||
    !Number.isSafeInteger(environment.generation) ||
    (environment.generation as number) < 1
  )
    throw new Error("Ting returned an invalid testing environment.");
  else if (
    environment.id !== expected.testing_environment_id ||
    environment.generation !== expected.testing_generation
  )
    throw new Error(
      "Ting and DM are using different testing environments or generations.",
    );
  return true;
}

/** Resolve the selected DM handle through this authenticated Ting session. */
function tingOrganization(value: unknown, selected: string): string {
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("Ting returned an invalid organization list.");
  const items = (value as { items?: unknown }).items;
  if (!Array.isArray(items) || items.length > 1000)
    throw new Error("Ting returned an invalid organization list.");
  const matches = items.filter(
    (item) =>
      item &&
      typeof item === "object" &&
      !Array.isArray(item) &&
      (item.id === selected || item.handle === selected),
  );
  if (
    matches.length !== 1 ||
    typeof matches[0].id !== "string" ||
    !matches[0].id.trim() ||
    matches[0].id.length > 255 ||
    /[\x00-\x20\x7f]/.test(matches[0].id)
  )
    throw new Error("Ting did not identify this organization uniquely.");
  return matches[0].id;
}

const transient = (reason: unknown) =>
  [
    "authorization_unavailable",
    "unavailable_authorization",
    "temporarily_unavailable",
    "rate_limited",
  ].includes(String(reason));
/** Match Ting's revalidated account and environment before watching. Hints only
 * invalidate DM state; independently authorized HTTP reads supply all content. */
export function watchTing(
  origin: string,
  actor: Actor,
  organization: string,
  environment: TingEnvironment,
  changed: () => void,
  status: (value: TingStatus) => void,
): { close(): void; reconnect(): void } {
  const base = new URL(origin);
  if (base.origin !== origin || !["http:", "https:"].includes(base.protocol))
    throw new Error("Ting browser origin is invalid.");
  const expected = { ...environment };
  if (
    expected.testing_environment_id === null
      ? expected.testing_generation !== null
      : typeof expected.testing_environment_id !== "string" ||
        !uuid.test(expected.testing_environment_id) ||
        !Number.isSafeInteger(expected.testing_generation) ||
        (expected.testing_generation ?? 0) < 1
  )
    throw new Error("DM has not verified this profile’s environment.");
  let stopped = false,
    epoch = 0,
    attempt = 0;
  let socket: WebSocket | undefined;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let abort: AbortController | undefined;
  let deadline: ReturnType<typeof setTimeout> | undefined;
  const emit = (state: TingState, message: string) =>
    status({ state, message, origin });
  const schedule = () => {
    if (stopped || retry) return;
    retry = setTimeout(
      () => {
        retry = undefined;
        void open();
      },
      Math.min(30_000, 1000 * 2 ** Math.min(attempt++, 5)),
    );
  };
  async function open() {
    if (stopped) return;
    const run = ++epoch;
    abort?.abort();
    abort = new AbortController();
    if (deadline) clearTimeout(deadline);
    deadline = setTimeout(() => abort?.abort(), 10_000);
    emit("connecting", "Checking your Ting sign-in…");
    try {
      const response = await fetch(new URL("/v1/me", base), {
        credentials: "include",
        cache: "no-store",
        redirect: "error",
        signal: abort.signal,
      });
      if (run !== epoch || stopped) return;
      if (response.status === 401) {
        if (deadline) clearTimeout(deadline);
        emit(
          "sign-in",
          "Sign in to Ting with this DM account, then reconnect.",
        );
        return;
      }
      if (!response.ok) throw new Error("Ting rejected the session check.");
      const me = await response.json();
      if (run !== epoch || stopped) return;
      if (deadline) clearTimeout(deadline);
      if (
        me?.authenticated !== true ||
        me.id !== actor.id ||
        me.kind !== actor.type
      ) {
        emit(
          "sign-in",
          "Ting must be signed in to the same Carbon or Silicon as this DM profile.",
        );
        return;
      }
      let verified: boolean;
      try {
        verified = verifyEnvironment(me.environment, expected);
      } catch (error) {
        emit(
          "blocked",
          `${error instanceof Error ? error.message : "Ting session context is invalid."} Sign in to Ting with this DM environment, then reconnect.`,
        );
        return;
      }
      deadline = setTimeout(() => abort?.abort(), 10_000);
      const organizations = await fetch(new URL("/v1/orgs", base), {
        credentials: "include",
        cache: "no-store",
        redirect: "error",
        signal: abort.signal,
      });
      if (run !== epoch || stopped) return;
      if (!organizations.ok)
        throw new Error("Ting could not verify organization access.");
      const listed = await organizations.json();
      if (run !== epoch || stopped) return;
      let tingOrg: string;
      try {
        tingOrg = tingOrganization(listed, organization);
      } catch {
        if (deadline) clearTimeout(deadline);
        emit(
          "blocked",
          "Ting could not verify this organization. Check your Ting organization access and reconnect.",
        );
        return;
      }
      if (deadline) clearTimeout(deadline);
      const url = new URL("/v1/ws?protocol=v1", base);
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      const target = new WebSocket(url);
      socket = target;
      deadline = setTimeout(
        () => target.close(1000, "ting-watch-timeout"),
        10_000,
      );
      const requestId = crypto.randomUUID();
      let watching = false;
      target.onmessage = (event) => {
        if (run !== epoch || stopped) return;
        if (typeof event.data !== "string" || event.data.length > 65536) {
          target.close(1008, "invalid-ting-hint");
          return;
        }
        let frame: any;
        try {
          frame = JSON.parse(event.data);
        } catch {
          target.close(1008, "invalid-ting-json");
          return;
        }
        if (
          !frame ||
          typeof frame !== "object" ||
          Array.isArray(frame) ||
          typeof frame.op !== "string"
        ) {
          target.close(1008, "invalid-ting-frame");
          return;
        }
        if (frame.op === "ready") {
          if (frame.protocol !== "v1") {
            target.close(1008, "unexpected-ting-protocol");
            return;
          }
          target.send(
            JSON.stringify({
              op: "watch_inbox",
              request_id: requestId,
              org_id: tingOrg,
            }),
          );
        } else if (
          frame.op === "watching_inbox" &&
          frame.org_id === tingOrg &&
          frame.request_id === requestId
        ) {
          if (deadline) clearTimeout(deadline);
          watching = true;
          attempt = 0;
          emit(
            verified ? "connected" : "unverified",
            verified
              ? "Ting connected with the matching DM account and environment."
              : "Ting hints connected. This Ting server did not disclose its session environment, so delivery context remains unverified.",
          );
          changed();
        } else if (
          frame.op === "inbox_changed" &&
          watching &&
          frame.org_id === tingOrg
        )
          changed();
        else if (frame.op === "paused" && frame.org_id === tingOrg) {
          watching = false;
          emit(
            "paused",
            "Ting paused these hints. DM history remains available; reconnect after checking your Ting permissions.",
          );
          // Revalidate transient authorization outages; auth/permission pauses require explicit recovery.
          if (deadline) clearTimeout(deadline);
          ++epoch;
          target.close(1000, "ting-paused");
          if (transient(frame.reason)) schedule();
        } else if (frame.op === "error") {
          watching = false;
          emit(
            "blocked",
            "Ting could not watch this inbox. Check its permissions and reconnect.",
          );
          if (deadline) clearTimeout(deadline);
          ++epoch;
          target.close(1000, "ting-rejected");
          if (transient(frame.error?.code)) schedule();
        }
        // Browser WebSocket handles protocol ping/pong. No Ting ACK or read is sent.
      };
      target.onerror = () => {
        if (run === epoch && !stopped)
          emit(
            "blocked",
            "Ting could not accept this website’s connection. Check Ting sign-in and allowed origins.",
          );
      };
      target.onclose = () => {
        if (run !== epoch || stopped) return;
        ++epoch;
        watching = false;
        if (deadline) clearTimeout(deadline);
        emit(
          "blocked",
          "Ting hints disconnected. DM is reconciling through its authorized history API.",
        );
        schedule();
      };
    } catch (error) {
      if (run !== epoch || stopped) return;
      if (deadline) clearTimeout(deadline);
      emit(
        "blocked",
        "Ting could not be reached from this website. Its browser origin/CORS settings may need updating; DM history remains available.",
      );
      schedule();
    }
  }
  void open();
  return {
    close() {
      stopped = true;
      ++epoch;
      abort?.abort();
      if (deadline) clearTimeout(deadline);
      if (retry) clearTimeout(retry);
      socket?.close(1000, "closed");
    },
    reconnect() {
      if (stopped) return;
      ++epoch;
      abort?.abort();
      if (retry) clearTimeout(retry);
      retry = undefined;
      socket?.close(1000, "reconnect");
      attempt = 0;
      void open();
    },
  };
}
