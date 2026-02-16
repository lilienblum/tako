import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

interface ViteUserConfigLike {
  server?: {
    host?: string;
    port?: number;
    strictPort?: boolean;
    allowedHosts?: true | string[];
  };
}

interface ViteConfigEnvLike {
  command?: "build" | "serve";
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
  apply?: "build" | "serve";
  config?: (_userConfig?: ViteUserConfigLike, env?: ViteConfigEnvLike) => ViteUserConfigLike;
  configResolved?: (config: ViteResolvedConfigLike) => void;
  generateBundle?: (_options: unknown, bundle: ViteOutputBundleLike) => void;
  closeBundle?: () => Promise<void>;
}

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
  return `import entryModule, * as entryNamespace from ${JSON.stringify(importSpecifier)};

const fetchHandler =
  typeof entryModule === "function"
    ? entryModule
    : entryModule && typeof entryModule.fetch === "function"
      ? entryModule.fetch.bind(entryModule)
      : typeof entryNamespace.fetch === "function"
        ? entryNamespace.fetch
        : null;

if (!fetchHandler) {
  throw new Error(
    "Invalid server entry: export a default fetch function, a default object with fetch, or a named fetch export.",
  );
}

export default fetchHandler;
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

function parsePortFromEnv(rawPort: string | undefined): number | null {
  const parsedPort = Number.parseInt((rawPort ?? "").trim(), 10);
  if (!Number.isInteger(parsedPort) || parsedPort <= 0 || parsedPort > 65535) {
    return null;
  }
  return parsedPort;
}

function mergeServeAllowedHosts(existing: true | string[] | undefined): true | string[] {
  if (existing === true) {
    return true;
  }

  const merged = Array.isArray(existing) ? [...existing] : [];
  if (!merged.includes(".tako.local")) {
    merged.push(".tako.local");
  }
  return merged;
}

export function takoVitePlugin(): VitePluginLike {
  let resolvedConfig: ViteResolvedConfigLike | null = null;
  let entryChunks: string[] = [];
  let sawBundleGeneration = false;
  let activeCommand: "build" | "serve" | null = null;

  return {
    name: "tako-vite-entry",
    config(_userConfig, env) {
      if (env?.command === "build" || env?.command === "serve") {
        activeCommand = env.command;
      }

      const config: ViteUserConfigLike = {};

      // Allow `tako dev` to reserve the local app port and have Vite bind there.
      if (activeCommand === "serve") {
        const serverConfig: NonNullable<ViteUserConfigLike["server"]> = {
          allowedHosts: mergeServeAllowedHosts(_userConfig?.server?.allowedHosts),
        };
        const parsedPort = parsePortFromEnv(process.env.PORT);
        if (parsedPort !== null) {
          serverConfig.host = "127.0.0.1";
          serverConfig.port = parsedPort;
          serverConfig.strictPort = true;
        }
        config.server = serverConfig;
      }

      return config;
    },
    configResolved(config) {
      resolvedConfig = config;
    },
    generateBundle(_options, bundle) {
      sawBundleGeneration = true;
      entryChunks = Object.values(bundle)
        .filter((chunk): chunk is ViteOutputChunkLike => {
          return (
            chunk.type === "chunk" && chunk.isEntry === true && typeof chunk.fileName === "string"
          );
        })
        .map((chunk) => chunk.fileName)
        .sort();
    },
    async closeBundle() {
      if (activeCommand === "serve") {
        return;
      }
      if (!resolvedConfig) {
        throw new Error("takoVitePlugin was not initialized by Vite configResolved hook.");
      }
      if (!sawBundleGeneration) {
        return;
      }

      const outDirAbs = path.isAbsolute(resolvedConfig.build.outDir)
        ? path.normalize(resolvedConfig.build.outDir)
        : path.resolve(resolvedConfig.root, resolvedConfig.build.outDir);
      const compiledMain = pickCompiledMain(entryChunks);
      const wrappedEntrySource = renderWrappedEntrySource(compiledMain);
      const wrappedEntryPath = path.resolve(outDirAbs, WRAPPED_ENTRY_FILE);

      await mkdir(path.dirname(wrappedEntryPath), { recursive: true });
      await writeFile(wrappedEntryPath, wrappedEntrySource, "utf8");
    },
  };
}
