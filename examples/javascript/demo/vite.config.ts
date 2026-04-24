import { tanstackStart } from "@tanstack/react-start/plugin/vite";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";
import { defineConfig } from "vite";
import viteReact from "@vitejs/plugin-react";
import { tako } from "tako.sh/vite";

export default defineConfig({
  plugins: [tanstackStart(), viteReact(), tailwindcss(), tako()],
  resolve: {
    alias: {
      "@": path.resolve(import.meta.dirname, "./src"),
    },
  },
  server: {
    allowedHosts: true,
  },
  optimizeDeps: {
    exclude: ["@resvg/resvg-js", "satori"],
  },
});
