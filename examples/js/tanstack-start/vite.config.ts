import { tanstackStart } from "@tanstack/react-start/plugin/vite";
import { defineConfig } from "vite";
import viteReact from "@vitejs/plugin-react";
import { takoVitePlugin } from "tako.sh/vite";

export default defineConfig({
  plugins: [tanstackStart({ srcDirectory: "src" }), viteReact(), takoVitePlugin()],
});
