import { defineConfig } from "tsdown";

export default defineConfig({
  entry: {
    index: "src/index.ts",
    vite: "src/vite.ts",
    nextjs: "src/nextjs/index.ts",
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
  deps: {
    onlyBundle: false,
    neverBundle: ["vite", "postcss"],
  },
});
