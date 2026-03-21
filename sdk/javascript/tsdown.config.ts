import { defineConfig } from "tsdown";

export default defineConfig({
  entry: {
    index: "src/index.ts",
    bun: "src/adapters/bun.ts",
    node: "src/adapters/node.ts",
    deno: "src/adapters/deno.ts",
    vite: "src/vite.ts",
    "entrypoints/bun": "src/entrypoints/bun.ts",
    "entrypoints/node": "src/entrypoints/node.ts",
    "entrypoints/deno": "src/entrypoints/deno.ts",
  },
  format: "esm",
  dts: true,
  minify: true,
  outDir: "dist",
  target: "esnext",
  platform: "node",
  clean: true,
});
