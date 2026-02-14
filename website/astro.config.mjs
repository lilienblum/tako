import { defineConfig } from "astro/config";
import { fileURLToPath } from "node:url";
import { SNIPPET_THEME } from "./src/config/snippet-theme.js";

const workspaceRoot = fileURLToPath(new URL("..", import.meta.url));

// Static build (dist/). Cloudflare Workers serves the assets and handles installer script headers.
export default defineConfig({
  output: "static",
  markdown: {
    shikiConfig: {
      theme: SNIPPET_THEME,
    },
  },
  vite: {
    server: {
      fs: {
        allow: [workspaceRoot],
      },
    },
  },
});
