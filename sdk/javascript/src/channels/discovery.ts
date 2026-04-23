import { readdir, stat } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { join, parse } from "node:path";
import { isChannelDefinition, isChannelExport, type ChannelDefinition } from "./define";

const VALID_EXTS = new Set([".ts", ".tsx", ".js", ".mjs", ".mts"]);

export interface DiscoveredChannel {
  name: string;
  definition: ChannelDefinition;
}

export async function discoverChannels(dir: string): Promise<DiscoveredChannel[]> {
  if (!(await dirExists(dir))) return [];

  const entries = await readdir(dir);
  const found: DiscoveredChannel[] = [];
  const seenPatterns = new Set<string>();

  for (const entry of entries.sort()) {
    const parsed = parse(entry);
    if (!VALID_EXTS.has(parsed.ext)) continue;
    if (parsed.name.startsWith(".") || parsed.name.startsWith("_")) continue;

    const url = pathToFileURL(join(dir, entry)).href;
    const mod = (await import(/* @vite-ignore */ url)) as Record<string, unknown>;
    const defaultExport = mod["default"];

    const definition: ChannelDefinition | undefined = isChannelExport(defaultExport)
      ? defaultExport.definition
      : isChannelDefinition(defaultExport)
        ? defaultExport
        : undefined;

    if (!definition) {
      throw new Error(
        `channel '${parsed.name}' (${entry}) must default-export a defineChannel() result`,
      );
    }

    if (seenPatterns.has(definition.pattern)) {
      throw new Error(`duplicate channel pattern '${definition.pattern}' in ${entry}`);
    }
    seenPatterns.add(definition.pattern);

    found.push({ name: parsed.name, definition });
  }

  return found;
}

async function dirExists(dir: string): Promise<boolean> {
  try {
    const s = await stat(dir);
    return s.isDirectory();
  } catch {
    return false;
  }
}
