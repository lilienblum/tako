import { defineConfig } from "astro/config";

// Static build (dist/). Cloudflare Workers serves the assets and handles installer script headers.
export default defineConfig({
  output: "static",
  vite: {
    // Allow the website to render docs sourced from repo-root markdown files (single source of truth).
    server: { fs: { allow: [".."] } },
  },
});
