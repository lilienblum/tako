import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

export interface TakoVitePluginOptions {
  /**
   * Output metadata file name written under Vite outDir.
   * Defaults to `.tako-vite.json`.
   */
  metadataFile?: string;
}

export interface TakoViteBuildMetadata {
  compiled_main: string;
  entries: string[];
}

interface ViteUserConfigLike {
  ssr?: {
    noExternal?: true | string | RegExp | Array<string | RegExp>;
  };
}

interface ViteResolvedConfigLike {
  root: string;
  build: {
    outDir: string;
  };
}

interface ViteOutputChunkLike {
  type: "chunk";
  fileName: string;
  isEntry: boolean;
}

interface ViteOutputBundleEntryLike {
  type: string;
  fileName?: string;
  isEntry?: boolean;
}

type ViteOutputBundleLike = Record<string, ViteOutputChunkLike | ViteOutputBundleEntryLike>;

export interface VitePluginLike {
  name: string;
  apply?: "build";
  config?: () => ViteUserConfigLike;
  configResolved?: (config: ViteResolvedConfigLike) => void;
  generateBundle?: (_options: unknown, bundle: ViteOutputBundleLike) => void;
  closeBundle?: () => Promise<void>;
}

const DEFAULT_METADATA_FILE = ".tako-vite.json";
const WRAPPED_ENTRY_FILE = "tako-entry.mjs";

function toPosixPath(filePath: string): string {
  return filePath.replaceAll("\\", "/");
}

function toRelativeImportSpecifier(filePath: string): string {
  const normalized = toPosixPath(filePath);
  if (normalized.startsWith("./") || normalized.startsWith("../")) {
    return normalized;
  }
  return `./${normalized}`;
}

function renderWrappedEntrySource(compiledMain: string): string {
  const importSpecifier = toRelativeImportSpecifier(compiledMain);
  return `import entryModule from ${JSON.stringify(importSpecifier)};

const startedAt = Date.now();
const TAKO_INTERNAL_HOST = "tako.internal";

function jsonResponse(payload, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function requestPathname(request) {
  try {
    return new URL(request.url).pathname;
  } catch {
    return "/";
  }
}

function normalizeHost(value) {
  if (typeof value !== "string") {
    return null;
  }
  const normalized = value.trim().toLowerCase();
  if (!normalized) {
    return null;
  }
  return normalized.split(":")[0];
}

function requestHost(request) {
  const headerHost = normalizeHost(request?.headers?.get?.("host"));
  if (headerHost) {
    return headerHost;
  }
  try {
    return normalizeHost(new URL(request.url).host);
  } catch {
    return null;
  }
}

function statusPayload() {
  const uptimeSeconds = Math.floor((Date.now() - startedAt) / 1000);
  const pid = typeof process !== "undefined" && typeof process.pid === "number" ? process.pid : null;
  return {
    status: "ok",
    pid,
    uptime_seconds: uptimeSeconds,
  };
}

const entryFetch =
  typeof entryModule === "function"
    ? entryModule
    : entryModule && typeof entryModule.fetch === "function"
      ? entryModule.fetch.bind(entryModule)
      : null;

export default {
  async fetch(request, ...rest) {
    const pathname = requestPathname(request);
    const host = requestHost(request);

    if (host === TAKO_INTERNAL_HOST && pathname === "/status") {
      return jsonResponse(statusPayload());
    }

    if (host === TAKO_INTERNAL_HOST) {
      return jsonResponse({ error: "Not found" }, 404);
    }

    if (!entryFetch) {
      return jsonResponse(
        {
          error:
            "Invalid server entry: default export must be fetch(request, ...args) (or legacy default object with fetch).",
        },
        500,
      );
    }

    try {
      return await entryFetch(request, ...rest);
    } catch (error) {
      console.error("[tako.sh] Wrapped server entry threw an error:", error);
      return jsonResponse({ error: "Internal Server Error" }, 500);
    }
  },
};
`;
}

function pickCompiledMain(entries: string[]): string {
  if (entries.length === 0) {
    throw new Error(
      "Could not detect server entry chunk in Vite build output. Ensure your SSR/server build emits an entry chunk.",
    );
  }

  if (entries.length === 1) {
    return entries[0];
  }

  const serverEntries = entries.filter((entry) =>
    entry
      .split("/")
      .map((segment) => segment.toLowerCase())
      .includes("server"),
  );

  if (serverEntries.length === 1) {
    return serverEntries[0];
  }

  throw new Error(
    `Could not choose a single server entry chunk from Vite output. Found: ${entries.join(", ")}. Configure your build to emit one server entry chunk.`,
  );
}

export function takoVitePlugin(options: TakoVitePluginOptions = {}): VitePluginLike {
  let resolvedConfig: ViteResolvedConfigLike | null = null;
  let entryChunks: string[] = [];

  return {
    name: "tako-vite-metadata",
    apply: "build",
    config() {
      // Deploy archives include built output only. Force SSR dependency bundling
      // so runtime startup does not depend on a separate node_modules tree.
      return {
        ssr: {
          noExternal: true,
        },
      };
    },
    configResolved(config) {
      resolvedConfig = config;
    },
    generateBundle(_options, bundle) {
      entryChunks = Object.values(bundle)
        .filter((chunk): chunk is ViteOutputChunkLike => {
          return chunk.type === "chunk" && chunk.isEntry === true && typeof chunk.fileName === "string";
        })
        .map((chunk) => chunk.fileName)
        .sort();
    },
    async closeBundle() {
      if (!resolvedConfig) {
        throw new Error("takoVitePlugin was not initialized by Vite configResolved hook.");
      }

      const outDirAbs = path.isAbsolute(resolvedConfig.build.outDir)
        ? path.normalize(resolvedConfig.build.outDir)
        : path.resolve(resolvedConfig.root, resolvedConfig.build.outDir);
      const metadataFile = options.metadataFile ?? DEFAULT_METADATA_FILE;
      const compiledMain = pickCompiledMain(entryChunks);
      const wrappedEntrySource = renderWrappedEntrySource(compiledMain);
      const wrappedEntryPath = path.resolve(outDirAbs, WRAPPED_ENTRY_FILE);
      const outDirBaseName = path.basename(outDirAbs).toLowerCase();

      const metadataPath =
        outDirBaseName === "server"
          ? path.resolve(path.dirname(outDirAbs), metadataFile)
          : path.resolve(outDirAbs, metadataFile);

      const mainForMetadata =
        outDirBaseName === "server"
          ? path.posix.join("server", WRAPPED_ENTRY_FILE)
          : WRAPPED_ENTRY_FILE;

      const metadata: TakoViteBuildMetadata = {
        compiled_main: mainForMetadata,
        entries: entryChunks,
      };

      await mkdir(path.dirname(wrappedEntryPath), { recursive: true });
      await writeFile(wrappedEntryPath, wrappedEntrySource, "utf8");
      await mkdir(path.dirname(metadataPath), { recursive: true });
      await writeFile(metadataPath, `${JSON.stringify(metadata, null, 2)}\n`, "utf8");
    },
  };
}
