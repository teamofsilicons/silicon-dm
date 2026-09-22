import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { build } from "esbuild";
const bundle = await build({
  entryPoints: [new URL("../src/delivery.ts", import.meta.url).pathname],
  bundle: true,
  write: false,
  format: "iife",
  globalName: "delivery",
  plugins: [
    {
      name: "fixture",
      setup(builder) {
        builder.onResolve({ filter: /^\.\/(api|storage)$/ }, (args) => ({
          path: args.path,
          namespace: "fixture",
        }));
        builder.onLoad({ filter: /.*/, namespace: "fixture" }, (args) => ({
          contents:
            args.path === "./api"
              ? `export const api=(...args)=>harness.api(...args);`
              : `
    export class StorageError extends Error{constructor(code,message){super(message);this.code=code;}}
    export const getGeneration=async()=>harness.generation;
    export const deliveryRegistrationKey=async()=>harness.key??=("key-"+(++harness.sequence));
    export const completeDeliveryRegistration=async(_s,_g,key)=>{if(harness.key===key)harness.key=undefined;};
  `,
        }));
      },
    },
  ],
});
test("uncertain enrollment retains its key until explicit new registration; success clears it", async () => {
  const calls: any[] = [],
    session = { profile_id: "p" };
  const harness: any = {
    generation: 7,
    sequence: 0,
    fail: true,
    api: async (path: string, options: any) => {
      calls.push({ path, options });
      if (harness.fail) throw new Error("uncertain");
      return { active: true };
    },
  };
  const ctx = vm.createContext({ harness });
  vm.runInContext(bundle.outputFiles[0]!.text, ctx);
  await assert.rejects(
    () => ctx.delivery.registerDelivery(session),
    /uncertain/,
  );
  const old = harness.key;
  assert.equal(calls.length, 1);
  await assert.rejects(
    () => ctx.delivery.registerDelivery(session),
    /uncertain/,
  );
  assert.equal(harness.key, old);
  assert.equal(calls[1].options.idempotencyKey, old);
  harness.fail = false;
  await ctx.delivery.registerDelivery(session, true);
  assert.notEqual(calls[2].options.idempotencyKey, old);
  assert.equal(calls[2].options.generation, 7);
  assert.equal(calls[2].options.session, session);
  assert.equal(harness.key, undefined);
});
test("unknown test generation never starts a registration attempt", async () => {
  const harness: any = {
    generation: undefined,
    sequence: 0,
    api: () => {
      throw new Error("must not call");
    },
  };
  const ctx = vm.createContext({ harness });
  vm.runInContext(bundle.outputFiles[0]!.text, ctx);
  await assert.rejects(
    () => ctx.delivery.registerDelivery({ profile_id: "p" }),
    /testing environment/,
  );
  assert.equal(harness.sequence, 0);
});
