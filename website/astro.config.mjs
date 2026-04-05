import { defineConfig } from "astro/config";
import { fileURLToPath } from "node:url";
import { SNIPPET_THEME } from "./src/config/snippet-theme.js";
import { remarkD2Theme } from "./src/remark/remark-d2-theme.js";
import astroD2 from "astro-d2";
import sitemap from "@astrojs/sitemap";

const workspaceRoot = fileURLToPath(new URL("..", import.meta.url));

// Static build (dist/). Cloudflare Workers serves the assets and handles installer script headers.
export default defineConfig({
  site: "https://tako.sh",
  output: "static",

  markdown: {
    remarkPlugins: [remarkD2Theme],
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
    astroD2({
      experimental: {
        useD2js: true,
      },
      sketch: true,
      theme: { default: "102", dark: false },
      pad: 40,
      skipGeneration: false,
    }),
    sitemap(),
  ],
});
