import { defineConfig } from "astro/config";

// Static build (dist/). Cloudflare Workers serves the assets and handles installer script headers.
export default defineConfig({
  output: "static",
});
