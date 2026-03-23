/**
 * Creates a Tako entrypoint for any JS runtime.
 *
 * Each runtime-specific entrypoint (bun.ts, node.ts, deno.ts) calls this
 * and only provides the server binding layer.
 *
 * CLI args (appended by tako-server):
 *   <main> --instance <id> --version <ver>
 */

import { readFileSync, closeSync } from "node:fs";
import { handleTakoEndpoint } from "./endpoints";
import { injectSecrets } from "./secrets";
import { Tako } from "./tako";
import type { FetchFunction, TakoStatus } from "./types";

// Read secrets from fd 3 (Tako runtime ABI).
// Must happen before parseArgs/import(main) so secrets are available
// from the very first line of user code.
try {
  const data = readFileSync(3, "utf-8");
  closeSync(3);
  try {
    const secrets = JSON.parse(data);
    injectSecrets(secrets);
  } catch {
    // fd 3 had data but it wasn't valid JSON — Tako launch path is broken
    console.error("Tako: invalid secrets JSON on fd 3");
    process.exit(1);
  }
} catch {
  // Any read error (EBADF, ENXIO, etc.) = not running under Tako — no secrets via fd
}

interface ParsedArgs {
  main: string;
  instance: string;
  version: string;
}

function parseArgs(argv: string[]): ParsedArgs {
  // argv: [runtime, entrypoint, main, --instance, id, --version, ver]
  // Skip argv[0] (runtime) and argv[1] (entrypoint script)
  const args = argv.slice(2);
  let main = "";
  let instance = "unknown";
  let version = "unknown";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--instance":
        instance = args[++i] ?? "unknown";
        break;
      case "--version":
        version = args[++i] ?? "unknown";
        break;
      default:
        if (!main && !args[i]?.startsWith("--")) {
          main = args[i] ?? "";
        }
        break;
    }
  }

  return { main, instance, version };
}

export function createEntrypoint() {
  const parsed = parseArgs(process.argv);
  const port = parseInt(process.env["PORT"] || "3000", 10);
  const host = process.env["HOST"] || "127.0.0.1";

  const startedAt = Date.now();
  let currentStatus: TakoStatus["status"] = "starting";

  function getStatus(): TakoStatus {
    return {
      status: currentStatus,
      app: "app",
      version: parsed.version,
      instance_id: parsed.instance,
      pid: process.pid,
      uptime_seconds: Math.floor((Date.now() - startedAt) / 1000),
    };
  }

  function setDraining(): void {
    currentStatus = "draining";
  }

  async function run(
    startServer: (handleRequest: (request: Request) => Promise<Response>) => void,
  ): Promise<void> {
    if (!parsed.main) {
      console.error("Usage: <runtime> entrypoint <main> [--instance <id>] [--version <ver>]");
      process.exit(1);
    }

    console.log("Starting application");

    let userFetch: FetchFunction;
    try {
      const module = await import(parsed.main);
      const defaultExport = module.default;
      if (typeof defaultExport === "function") {
        userFetch = defaultExport as FetchFunction;
      } else if (
        defaultExport &&
        typeof defaultExport === "object" &&
        typeof defaultExport.fetch === "function"
      ) {
        userFetch = defaultExport.fetch as FetchFunction;
      } else {
        throw new Error("App must export a default fetch function or { fetch } object.");
      }
    } catch (err) {
      console.error(`Failed to import app from ${parsed.main}:`, err);
      process.exit(1);
    }

    Tako.getInstance();

    const env: Record<string, string> = {};
    for (const [key, value] of Object.entries(process.env)) {
      if (value !== undefined) {
        env[key] = value;
      }
    }

    const handleRequest = async (request: Request): Promise<Response> => {
      const takoResponse = await handleTakoEndpoint(request, getStatus());
      if (takoResponse) {
        return takoResponse;
      }

      try {
        return await userFetch(request, env);
      } catch (err) {
        console.error("Error in user fetch handler:", err);
        return new Response(JSON.stringify({ error: "Internal Server Error" }), {
          status: 500,
          headers: { "Content-Type": "application/json" },
        });
      }
    };

    startServer(handleRequest);
    currentStatus = "healthy";
    console.log("Entrypoint initialized successfully");
  }

  return { run, host, port, setDraining };
}
