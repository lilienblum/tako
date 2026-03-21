/**
 * Creates a Tako entrypoint for any JS runtime.
 *
 * Each runtime-specific entrypoint (bun.ts, node.ts, deno.ts) calls this
 * and only provides the server binding layer.
 *
 * CLI args (appended by tako-server):
 *   <main> --socket <path> --instance <id> --version <ver>
 */

import { handleTakoEndpoint } from "./endpoints";
import { resolveAppSocketPath } from "./socket-path";
import { Tako } from "./tako";
import type { FetchFunction, TakoStatus } from "./types";

interface ParsedArgs {
  main: string;
  socket: string | undefined;
  instance: string;
  version: string;
}

function parseArgs(argv: string[]): ParsedArgs {
  // argv: [runtime, entrypoint, main, --socket, path, --instance, id, --version, ver]
  // Skip argv[0] (runtime) and argv[1] (entrypoint script)
  const args = argv.slice(2);
  let main = "";
  let socket: string | undefined;
  let instance = "unknown";
  let version = "unknown";

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--socket":
        socket = args[++i] ?? "";
        break;
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

  return { main, socket, instance, version };
}

export function createEntrypoint() {
  const parsed = parseArgs(process.argv);
  const appSocketPath = resolveAppSocketPath(parsed.socket);
  const port = parseInt(process.env["PORT"] || "3000", 10);

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
      console.error(
        "Usage: <runtime> entrypoint <main> [--socket <path>] [--instance <id>] [--version <ver>]",
      );
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

  return { run, appSocketPath, port, setDraining };
}
