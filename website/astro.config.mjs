import { defineConfig } from "astro/config";
import { fileURLToPath } from "node:url";
import { SNIPPET_THEME } from "./src/config/snippet-theme.js";
import rehypeMermaid from "rehype-mermaid";
import sitemap from "@astrojs/sitemap";

const workspaceRoot = fileURLToPath(new URL("..", import.meta.url));

// Static build (dist/). Cloudflare Workers serves the assets and handles installer script headers.
export default defineConfig({
  site: "https://tako.sh",
  output: "static",

  markdown: {
    shikiConfig: {
      theme: SNIPPET_THEME,
      excludeLangs: ["mermaid"],
    },
    rehypePlugins: [[rehypeMermaid, { strategy: "inline-svg" }]],
  },

  vite: {
    server: {
      fs: {
        allow: [workspaceRoot],
      },
    },
  },

  integrations: [sitemap()],
});
