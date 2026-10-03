import type { Session } from "./models";

type Pending = {
  authorization_id: string;
  authorization_url: string;
  status: "pending";
  expires_at: string;
};
export type AuthorizationState = {
  request?: Pending;
  code: string;
  busy: boolean;
  error: string;
};
type Transport = (
  path: string,
  options: {
    session: Session;
    method: string;
    body: unknown;
    idempotencyKey: string;
  },
) => Promise<unknown>;
const uuid =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

/** One explicit consent attempt, bound to one immutable DM account and organization. */
export class AuthorizationFlow {
  state: AuthorizationState = { code: "", busy: false, error: "" };
  private epoch = 0;
  private alive = true;
  private startKey = crypto.randomUUID();
  private completion?: { code: string; key: string };
  constructor(
    private session: Session,
    private send: Transport,
    private changed: (state: AuthorizationState) => void,
    private enabled: () => void,
  ) {
    this.session = structuredClone(session);
  }
  private publish(patch: Partial<AuthorizationState>) {
    this.state = { ...this.state, ...patch };
    this.changed(this.state);
  }
  reset(session = this.session) {
    this.epoch++;
    this.session = structuredClone(session);
    this.startKey = crypto.randomUUID();
    this.completion = undefined;
    this.publish({ request: undefined, code: "", busy: false, error: "" });
  }
  dispose() {
    this.alive = false;
    this.epoch++;
    this.completion = undefined;
    this.state = { code: "", busy: false, error: "" };
  }
  setCode(code: string) {
    this.publish({ code });
  }
  private current(epoch: number) {
    return this.alive && this.epoch === epoch;
  }
  private failed(error: unknown) {
    const e = error as { status?: number; message?: string; code?: string };
    if ([412, 428].includes(e.status || 0) || e.code === "environment_changed") {
      this.reset();
      this.publish({
        error:
          "This permission review expired or changed. Start a new review in IAM.",
      });
    } else
      this.publish({ error: e.message || "Authorization could not complete." });
  }
  async start() {
    if (this.state.busy || !this.alive) return;
    const epoch = this.epoch,
      session = this.session;
    this.publish({ busy: true, error: "" });
    try {
      const value = (await this.send("/delivery/authorization", {
        session,
        method: "POST",
        body: {},
        idempotencyKey: this.startKey,
      })) as Pending;
      if (!this.current(epoch)) return;
      const url = new URL(value.authorization_url);
      if (
        !uuid.test(value.authorization_id) ||
        value.status !== "pending" ||
        !(Date.parse(value.expires_at) > Date.now()) ||
        url.username ||
        url.password ||
        (url.protocol !== "https:" &&
          !(
            url.protocol === "http:" &&
            ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)
          ))
      )
        throw Error(
          "IAM returned an invalid or expired permission review. Start again.",
        );
      this.publish({ request: value });
    } catch (e) {
      if (this.current(epoch)) this.failed(e);
    } finally {
      if (this.current(epoch)) this.publish({ busy: false });
    }
  }
  async finish() {
    const pending = this.state.request,
      code = this.state.code.trim();
    if (this.state.busy || !pending || !code || !this.alive) return;
    if (Date.parse(pending.expires_at) <= Date.now()) {
      this.failed({ status: 412 });
      return;
    }
    if (this.completion?.code !== code)
      this.completion = { code, key: crypto.randomUUID() };
    const epoch = this.epoch,
      session = this.session;
    this.publish({ busy: true, error: "" });
    try {
      const result = (await this.send("/delivery/authorization/complete", {
        session,
        method: "POST",
        body: {
          authorization_id: pending.authorization_id,
          authorization_code: code,
        },
        idempotencyKey: this.completion.key,
      })) as { status?: string; authorization_id?: string };
      if (
        !this.current(epoch) ||
        this.state.request?.authorization_id !== pending.authorization_id
      )
        return;
      if (
        result.status !== "authorized" ||
        (result.authorization_id !== undefined &&
          result.authorization_id !== pending.authorization_id)
      )
        throw Error(
          "IAM did not confirm this permission review. Retry or start a new review.",
        );
      this.reset();
      this.enabled();
    } catch (e) {
      if (this.current(epoch)) this.failed(e);
    } finally {
      if (this.current(epoch)) this.publish({ busy: false });
    }
  }
}
