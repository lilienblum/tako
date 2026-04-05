import path from "node:path";

import type { NextjsBuildManifest } from "./types";

export const TAKO_NEXTJS_ENTRYPOINT = "tako-entry.mjs";

export function createNextjsBuildManifest(
  projectDir: string,
  distDir: string,
): NextjsBuildManifest {
  const distRoot = path.resolve(projectDir, distDir);
  const standaloneDir = path.join(distRoot, "standalone");

  return {
    distRoot,
    takoEntrypoint: path.join(distRoot, TAKO_NEXTJS_ENTRYPOINT),
    standaloneDir,
    standaloneServer: path.join(standaloneDir, "server.js"),
    staticDir: path.join(distRoot, "static"),
    publicDir: path.join(projectDir, "public"),
    standaloneStaticDir: path.join(standaloneDir, ".next", "static"),
    standalonePublicDir: path.join(standaloneDir, "public"),
  };
}

export function nextjsEntrypointContents(): string {
  return [
    'import { createNextjsFetchHandler } from "tako.sh/nextjs";',
    "",
    'import { access } from "node:fs/promises";',
    "",
    'const standaloneServer = new URL("./standalone/server.js", import.meta.url);',
    'const nextBin = new URL("../node_modules/next/dist/bin/next", import.meta.url);',
    "",
    "async function resolveHandler() {",
    "  try {",
    "    await access(standaloneServer);",
    "    return createNextjsFetchHandler(standaloneServer);",
    "  } catch {",
    "    return createNextjsFetchHandler(nextBin, {",
    '      argv: ["start"],',
    '      cwd: new URL("..", import.meta.url),',
    "    });",
    "  }",
    "}",
    "",
    "const handlerPromise = resolveHandler();",
    "",
    "async function ready() {",
    "  const handler = await handlerPromise;",
    '  if (typeof handler.ready === "function") {',
    "    await handler.ready();",
    "  }",
    "}",
    "",
    "async function fetch(request, env) {",
    "  const handler = await handlerPromise;",
    "  return await handler(request, env);",
    "}",
    "",
    "fetch.ready = ready;",
    "",
    "export default fetch;",
    "",
  ].join("\n");
}
