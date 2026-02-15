import { mkdir, readdir, rm, stat, copyFile } from "node:fs/promises";
import path from "node:path";

export interface TakoVitePluginOptions {
  /**
   * Destination directory for deployable artifacts.
   * Defaults to `.tako/artifacts/app`.
   */
  artifactDir?: string;
  /**
   * Client output directory to merge with public assets.
   * If omitted, Tako tries to auto-detect it.
   */
  clientDir?: string;
  /**
   * Server output directory to copy into artifacts.
   * Set to `false` to disable server copy.
   */
  serverDir?: string | false;
  /**
   * Override Vite public directory.
   * Set to `false` to skip public merge.
   */
  publicDir?: string | false;
  /**
   * Subdirectory inside artifacts for static assets.
   * Defaults to `static`.
   */
  staticSubdir?: string;
  /**
   * Subdirectory inside artifacts for server output.
   * Defaults to `server`.
   */
  serverSubdir?: string;
  /**
   * Remove existing artifact directory before staging.
   * Defaults to `true`.
   */
  cleanArtifactDir?: boolean;
}

export interface StageTakoViteArtifactsArgs {
  root: string;
  outDir: string;
  publicDir: string | false;
  options?: TakoVitePluginOptions;
}

export interface StageTakoViteArtifactsResult {
  artifactDir: string;
  staticDir: string;
  clientDir: string;
  serverDir: string | null;
}

interface ViteResolvedConfigLike {
  root: string;
  publicDir: string | false;
  build: {
    outDir: string;
  };
}

export interface VitePluginLike {
  name: string;
  apply?: "build";
  configResolved?: (config: ViteResolvedConfigLike) => void;
  closeBundle?: () => Promise<void>;
}

function resolvePath(root: string, maybeRelative: string): string {
  return path.isAbsolute(maybeRelative)
    ? path.normalize(maybeRelative)
    : path.resolve(root, maybeRelative);
}

async function isDirectory(absPath: string): Promise<boolean> {
  try {
    const fileStat = await stat(absPath);
    return fileStat.isDirectory();
  } catch {
    return false;
  }
}

async function looksLikeClientOutput(dir: string): Promise<boolean> {
  try {
    const entries = await readdir(dir, { withFileTypes: true });
    if (entries.some((entry) => entry.name === "assets" && entry.isDirectory())) {
      return true;
    }
    if (entries.some((entry) => entry.name === "index.html" && entry.isFile())) {
      return true;
    }
    if (entries.some((entry) => entry.name === "manifest.json" && entry.isFile())) {
      return true;
    }
    return false;
  } catch {
    return false;
  }
}

function unique(values: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const value of values) {
    if (!seen.has(value)) {
      seen.add(value);
      out.push(value);
    }
  }
  return out;
}

async function detectClientDir(
  root: string,
  outDirAbs: string,
  options: TakoVitePluginOptions,
): Promise<string> {
  if (options.clientDir) {
    const abs = resolvePath(root, options.clientDir);
    if (!(await isDirectory(abs))) {
      throw new Error(
        `Configured clientDir does not exist or is not a directory: ${options.clientDir}`,
      );
    }
    return abs;
  }

  const candidates = unique([
    path.join(outDirAbs, "client"),
    path.join(outDirAbs, "web"),
    path.join(outDirAbs, "browser"),
    path.resolve(root, "dist/client"),
    path.resolve(root, "build/client"),
    outDirAbs,
    path.resolve(root, "dist"),
    path.resolve(root, "build"),
  ]);

  for (const candidate of candidates) {
    if ((await isDirectory(candidate)) && (await looksLikeClientOutput(candidate))) {
      return candidate;
    }
  }

  throw new Error(
    "Could not detect Vite client output directory. Set `clientDir` in `takoVitePlugin({...})`.",
  );
}

async function detectServerDir(
  root: string,
  outDirAbs: string,
  options: TakoVitePluginOptions,
): Promise<string | null> {
  if (options.serverDir === false) {
    return null;
  }

  if (typeof options.serverDir === "string" && options.serverDir.length > 0) {
    const abs = resolvePath(root, options.serverDir);
    if (!(await isDirectory(abs))) {
      throw new Error(
        `Configured serverDir does not exist or is not a directory: ${options.serverDir}`,
      );
    }
    return abs;
  }

  const candidates = unique([
    path.join(outDirAbs, "server"),
    path.resolve(root, "dist/server"),
    path.resolve(root, "build/server"),
  ]);

  for (const candidate of candidates) {
    if (await isDirectory(candidate)) {
      return candidate;
    }
  }

  return null;
}

async function copyDirectoryContents(sourceDir: string, targetDir: string): Promise<void> {
  await mkdir(targetDir, { recursive: true });

  const entries = await readdir(sourceDir, { withFileTypes: true });
  for (const entry of entries) {
    const sourcePath = path.join(sourceDir, entry.name);
    const targetPath = path.join(targetDir, entry.name);

    if (entry.isDirectory()) {
      await copyDirectoryContents(sourcePath, targetPath);
      continue;
    }

    if (entry.isFile()) {
      await mkdir(path.dirname(targetPath), { recursive: true });
      await copyFile(sourcePath, targetPath);
      continue;
    }

    if (entry.isSymbolicLink()) {
      throw new Error(`Symbolic links are not supported in staged artifacts: ${sourcePath}`);
    }
  }
}

export async function stageTakoViteArtifacts(
  args: StageTakoViteArtifactsArgs,
): Promise<StageTakoViteArtifactsResult> {
  const options = args.options ?? {};
  const root = path.resolve(args.root);
  const outDirAbs = resolvePath(root, args.outDir);
  const artifactDir = resolvePath(
    root,
    options.artifactDir ?? path.join(".tako", "artifacts", "app"),
  );
  const staticDir = path.join(artifactDir, options.staticSubdir ?? "static");
  const serverStageDir = path.join(artifactDir, options.serverSubdir ?? "server");

  if (options.cleanArtifactDir !== false) {
    await rm(artifactDir, { recursive: true, force: true });
  }
  await mkdir(artifactDir, { recursive: true });

  const clientDir = await detectClientDir(root, outDirAbs, options);
  const serverDir = await detectServerDir(root, outDirAbs, options);

  const publicDirConfig = options.publicDir ?? args.publicDir;
  if (publicDirConfig !== false) {
    const publicDir = resolvePath(root, publicDirConfig);
    if (await isDirectory(publicDir)) {
      await copyDirectoryContents(publicDir, staticDir);
    }
  }

  await copyDirectoryContents(clientDir, staticDir);

  if (serverDir) {
    await copyDirectoryContents(serverDir, serverStageDir);
  }

  return {
    artifactDir,
    staticDir,
    clientDir,
    serverDir,
  };
}

export function takoVitePlugin(options: TakoVitePluginOptions = {}): VitePluginLike {
  let resolvedConfig: ViteResolvedConfigLike | null = null;

  return {
    name: "tako-vite-artifacts",
    apply: "build",
    configResolved(config) {
      resolvedConfig = config;
    },
    async closeBundle() {
      if (!resolvedConfig) {
        throw new Error("takoVitePlugin was not initialized by Vite configResolved hook.");
      }

      await stageTakoViteArtifacts({
        root: resolvedConfig.root,
        outDir: resolvedConfig.build.outDir,
        publicDir: resolvedConfig.publicDir,
        options,
      });
    },
  };
}
