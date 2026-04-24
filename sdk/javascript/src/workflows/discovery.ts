/**
 * Filesystem discovery for workflows/ directory.
 *
 * Each `<name>.(ts|tsx|js|mjs|mts)` file becomes a workflow named `<name>`.
 * The default export must be either:
 *   - A `WorkflowDefinition` produced by `defineWorkflow(fn, config?)` — handler
 *     and config are read directly from the object.
 *   - A plain function — registered with empty config.
 *
 * Nested directories are not scanned in v1 — flat structure only.
 */

import { readdir, stat } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { join, parse } from "node:path";
import { dynImport } from "../tako/dyn-import";
import { isWorkflowDefinition, isWorkflowExport } from "./define";
import type { WorkflowConfig } from "./types";
import type { WorkflowHandler } from "./worker";

const VALID_EXTS = new Set([".ts", ".tsx", ".js", ".mjs", ".mts"]);

export interface DiscoveredWorkflow {
  name: string;
  handler: WorkflowHandler;
  config: WorkflowConfig;
}

export async function discoverWorkflows(dir: string): Promise<DiscoveredWorkflow[]> {
  const exists = await dirExists(dir);
  if (!exists) return [];

  const entries = await readdir(dir);
  const found: DiscoveredWorkflow[] = [];
  for (const entry of entries) {
    const parsed = parse(entry);
    if (!VALID_EXTS.has(parsed.ext)) continue;
    if (parsed.name.startsWith(".") || parsed.name.startsWith("_")) continue;

    const url = pathToFileURL(join(dir, entry)).href;
    const mod = (await dynImport(url)) as Record<string, unknown>;
    const defaultExport = mod["default"];

    if (isWorkflowExport(defaultExport)) {
      const def = defaultExport.definition;
      if (def.name !== parsed.name) {
        throw new Error(
          `workflow file '${parsed.name}' exports defineWorkflow('${def.name}', ...); the name must match the file basename`,
        );
      }
      found.push({ name: def.name, handler: def.handler, config: def.config });
    } else if (isWorkflowDefinition(defaultExport)) {
      found.push({
        name: defaultExport.name,
        handler: defaultExport.handler,
        config: defaultExport.config,
      });
    } else if (typeof defaultExport === "function") {
      found.push({
        name: parsed.name,
        handler: defaultExport as WorkflowHandler,
        config: {},
      });
    } else {
      throw new Error(
        `workflow '${parsed.name}' (${entry}) must default-export a defineWorkflow() result or a plain function`,
      );
    }
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
