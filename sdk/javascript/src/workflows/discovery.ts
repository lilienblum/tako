/**
 * Filesystem discovery for workflows/ directory.
 *
 * Each `<name>.(ts|js|mjs|mts)` file becomes a workflow named `<name>`.
 * Default export is the handler. Named exports populate WorkflowConfig:
 *   - `schedule`     → cron expression
 *   - `maxAttempts`  → number
 *   - `concurrency`  → number (v1: recorded but single-concurrency only)
 *   - `timeoutMs`    → number (v1: recorded, not enforced)
 *
 * Nested directories are not scanned in v1 — flat structure only.
 */

import { readdir, stat } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { join, parse } from "node:path";
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
    const mod = (await import(url)) as Record<string, unknown>;

    const handler = mod["default"];
    if (typeof handler !== "function") {
      throw new Error(`workflow '${parsed.name}' (${entry}) must default-export a function`);
    }

    const config = extractConfig(mod);
    found.push({ name: parsed.name, handler: handler as WorkflowHandler, config });
  }
  return found;
}

function extractConfig(mod: Record<string, unknown>): WorkflowConfig {
  const cfg: WorkflowConfig = {};
  if (typeof mod["schedule"] === "string") cfg.schedule = mod["schedule"];
  if (typeof mod["maxAttempts"] === "number") cfg.maxAttempts = mod["maxAttempts"];
  if (typeof mod["concurrency"] === "number") cfg.concurrency = mod["concurrency"];
  if (typeof mod["timeoutMs"] === "number") cfg.timeoutMs = mod["timeoutMs"];
  const backoff = mod["backoff"];
  if (backoff && typeof backoff === "object") {
    const b = backoff as { base?: unknown; max?: unknown };
    const out: { base?: number; max?: number } = {};
    if (typeof b.base === "number") out.base = b.base;
    if (typeof b.max === "number") out.max = b.max;
    cfg.backoff = out;
  }
  return cfg;
}

async function dirExists(dir: string): Promise<boolean> {
  try {
    const s = await stat(dir);
    return s.isDirectory();
  } catch {
    return false;
  }
}
