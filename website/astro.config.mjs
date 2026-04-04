import { defineConfig } from "astro/config";
import { fileURLToPath } from "node:url";
import { SNIPPET_THEME } from "./src/config/snippet-theme.js";
import astroD2 from "astro-d2";
import sitemap from "@astrojs/sitemap";

const workspaceRoot = fileURLToPath(new URL("..", import.meta.url));

// Static build (dist/). Cloudflare Workers serves the assets and handles installer script headers.
export default defineConfig({
  site: "https://tako.sh",
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

  integrations: [
    astroD2({ sketch: true, theme: { light: "102", dark: "200" }, pad: 40 }),
    sitemap(),
  ],
});
