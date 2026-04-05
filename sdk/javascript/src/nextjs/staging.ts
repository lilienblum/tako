import { access, cp, mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import { createNextjsBuildManifest, nextjsEntrypointContents } from "./manifest";

export async function stageNextjsBuildOutput(projectDir: string, distDir: string): Promise<void> {
  const manifest = createNextjsBuildManifest(projectDir, distDir);

  if (await pathExists(manifest.standaloneServer)) {
    if (await pathExists(manifest.staticDir)) {
      await mkdir(path.dirname(manifest.standaloneStaticDir), { recursive: true });
      await copyDirectory(manifest.staticDir, manifest.standaloneStaticDir);
    }
    if (await pathExists(manifest.publicDir)) {
      await copyDirectory(manifest.publicDir, manifest.standalonePublicDir);
    }
  }

  await writeFile(manifest.takoEntrypoint, nextjsEntrypointContents(), "utf8");
}

async function pathExists(targetPath: string): Promise<boolean> {
  try {
    await access(targetPath);
    return true;
  } catch {
    return false;
  }
}

async function copyDirectory(source: string, destination: string): Promise<void> {
  await cp(source, destination, {
    recursive: true,
    force: true,
  });
}
