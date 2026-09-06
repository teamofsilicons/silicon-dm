import { build } from "esbuild";

await build({
  entryPoints: ["server/node.ts"],
  outfile: "dist/server/node.js",
  bundle: true,
  platform: "node",
  format: "esm",
  packages: "external",
  target: "node24",
  sourcemap: false,
});
