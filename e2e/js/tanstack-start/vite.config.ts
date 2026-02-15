import { tanstackStart } from "@tanstack/react-start/plugin/vite";
import { defineConfig } from "vite";
import tsConfigPaths from "vite-tsconfig-paths";
import viteReact from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { nitro } from "nitro/vite";
import { takoVitePlugin } from "tako.sh/vite";

export default defineConfig({
  server: {
    port: 3000,
  },
  plugins: [
    tailwindcss(),
    tsConfigPaths({ projects: ["./tsconfig.json"] }),
    tanstackStart({ srcDirectory: "src" }),
    viteReact(),
    nitro(),
    takoVitePlugin({
      clientDir: ".output/public",
      serverDir: ".output/server",
    }),
  ],
});
