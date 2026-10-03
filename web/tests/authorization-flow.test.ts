import assert from "node:assert/strict";
import test from "node:test";
import { AuthorizationFlow } from "../src/authorization-flow.ts";
import type { Session } from "../src/models.ts";
const session = {
  authenticated: true,
  profile_id: "profile-a",
  organization_id: "alpha",
  actor: { id: "c:alice", type: "carbon" },
  profiles: [],
} as Session;
const pending = () => ({
  authorization_id: crypto.randomUUID(),
  authorization_url: "https://auth.iam.example/review",
  status: "pending",
  expires_at: new Date(Date.now() + 60_000).toISOString(),
});

test("consent retries retain exact operation keys and rotate them after successful completion", async () => {
  const calls: {
    path: string;
    key: string;
    body: unknown;
    session: Session;
  }[] = [];
  let starts = 0,
    finishes = 0,
    enabled = 0;
  const flow = new AuthorizationFlow(
    session,
    async (path, options) => {
      calls.push({
        path,
        key: options.idempotencyKey,
        body: options.body,
        session: options.session,
      });
      if (path.endsWith("/complete")) {
        if (!finishes++) throw Error("Network interrupted");
        return { status: "authorized" };
      }
      if (!starts++) throw Error("Network interrupted");
      return pending();
    },
    () => {},
    () => enabled++,
  );
  await flow.start();
  await flow.start();
  assert.equal(calls[0]!.key, calls[1]!.key);
  flow.setCode("secret-code");
  await flow.finish();
  await flow.finish();
  assert.equal(calls[2]!.key, calls[3]!.key);
  assert.deepEqual(calls[2]!.body, calls[3]!.body);
  assert.equal(enabled, 1);
  assert.equal(flow.state.code, "");
  await flow.start();
  assert.notEqual(calls[4]!.key, calls[0]!.key);
  assert(calls.every((c) => c.session.profile_id === "profile-a"));
});

test("expired permission reviews require a new start and completion state must be authorized", async () => {
  const keys: string[] = [];
  let finishResponse: unknown = { status: "pending" };
  const flow = new AuthorizationFlow(
    session,
    async (path, options) => {
      keys.push(options.idempotencyKey);
      if (path.endsWith("/complete")) {
        if (finishResponse instanceof Error) throw finishResponse;
        return finishResponse;
      }
      return pending();
    },
    () => {},
    () => assert.fail("must not enable"),
  );
  await flow.start();
  flow.setCode("code");
  await flow.finish();
  assert.match(flow.state.error, /did not confirm/);
  finishResponse = Object.assign(Error("Review changed"), { status: 412 });
  await flow.finish();
  assert.equal(flow.state.request, undefined);
  assert.equal(flow.state.code, "");
  await flow.start();
  assert.notEqual(keys[0], keys.at(-1));
});

test("late starts and completions cannot update a switched or unmounted consent flow", async () => {
  let release!: (value: unknown) => void;
  let enabled = 0;
  const flow = new AuthorizationFlow(
    session,
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
    () => {},
    () => enabled++,
  );
  const first = flow.start();
  flow.reset({ ...session, profile_id: "profile-b", organization_id: "beta" });
  release(pending());
  await first;
  assert.equal(flow.state.request, undefined);
  const second = flow.start();
  release(pending());
  await second;
  flow.setCode("code");
  const completion = flow.finish();
  flow.dispose();
  release({ status: "authorized" });
  await completion;
  assert.equal(enabled, 0);
  assert.equal(flow.state.code, "");
});

test("review URLs reject embedded credentials and mismatched completion ids", async () => {
  let review = {
    ...pending(),
    authorization_url: "https://name:password@iam.example/review",
  };
  const flow = new AuthorizationFlow(
    session,
    async (path) =>
      path.endsWith("/complete")
        ? { status: "authorized", authorization_id: crypto.randomUUID() }
        : review,
    () => {},
    () => assert.fail("must not enable"),
  );
  await flow.start();
  assert.equal(flow.state.request, undefined);
  assert.match(flow.state.error, /invalid/);
  review = pending();
  await flow.start();
  flow.setCode("code");
  await flow.finish();
  assert.match(flow.state.error, /did not confirm/);
});
