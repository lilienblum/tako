import { defineConfig } from "astro/config";
import { fileURLToPath } from "node:url";

const workspaceRoot = fileURLToPath(new URL("..", import.meta.url));

// Static build (dist/). Cloudflare Workers serves the assets and handles installer script headers.
export default defineConfig({
  output: "static",
  vite: {
    server: {
      fs: {
        allow: [workspaceRoot],
      },
    },
  },
});
