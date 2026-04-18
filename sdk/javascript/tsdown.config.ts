import { defineConfig } from "tsdown";

export default defineConfig({
  entry: {
    index: "src/index.ts",
    client: "src/client.ts",
    react: "src/react.ts",
    vite: "src/vite.ts",
    server: "src/server.ts",
    internal: "src/internal.ts",
    nextjs: "src/nextjs/index.ts",
    "entrypoints/bun-server": "src/tako/entrypoints/bun-server.ts",
    "entrypoints/node-server": "src/tako/entrypoints/node-server.ts",
    "entrypoints/deno-server": "src/tako/entrypoints/deno-server.ts",
    "entrypoints/bun-worker": "src/tako/entrypoints/bun-worker.ts",
    "entrypoints/node-worker": "src/tako/entrypoints/node-worker.ts",
    "entrypoints/deno-worker": "src/tako/entrypoints/deno-worker.ts",
    "entrypoints/bun-dev": "src/tako/entrypoints/bun-dev.ts",
    "entrypoints/node-dev": "src/tako/entrypoints/node-dev.ts",
    "entrypoints/deno-dev": "src/tako/entrypoints/deno-dev.ts",
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
    neverBundle: ["vite", "react", "react-dom"],
  },
});
