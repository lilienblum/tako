/**
 * Creates a Tako entrypoint for any JS runtime.
 *
 * Each runtime-specific entrypoint (bun.ts, node.ts, deno.ts) calls this
 * and only provides the server binding layer.
 *
 * CLI args (appended by tako-server):
 *   <main> --instance <id> --version <ver>
 */

import { readFileSync, closeSync, fstatSync, writeSync } from "node:fs";
import { handleTakoEndpoint } from "./endpoints";
import { injectSecrets } from "./secrets";
import { installTakoGlobal } from "./tako";
import type { FetchFunction, ReadyableFetchHandler, TakoStatus } from "./types";

function readSecretsFromFd(): void {
  try {
    // Check fd 3 is a pipe (Tako passes secrets this way).
    // Without this guard, readFileSync(3) blocks forever if fd 3 is
    // open but not a Tako pipe (e.g. GitHub Actions runner logging fd).
    const stat = fstatSync(3);
    if (!stat.isFIFO()) return;

    const data = readFileSync(3, "utf-8");
    closeSync(3);
    try {
      const secrets = JSON.parse(data);
      if (typeof secrets !== "object" || secrets === null || Array.isArray(secrets)) {
        console.error("Tako: secrets on fd 3 must be a JSON object");
        process.exit(1);
      }
      injectSecrets(Object.assign(Object.create(null), secrets));
    } catch {
      console.error("Tako: invalid secrets JSON on fd 3");
      process.exit(1);
    }
  } catch {
    // Any error (EBADF, ENXIO, etc.) = not running under Tako
  }
}

export function signalReadyPortOnFd(fd: number, port: number): void {
  try {
    const stat = fstatSync(fd);
    if (!stat.isFIFO()) return;

    writeSync(fd, `${port}\n`);
    closeSync(fd);
  } catch {
    // Not running under Tako or readiness pipe unavailable.
  }
}

function signalReadyPort(port: number): void {
  signalReadyPortOnFd(4, port);
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
  readSecretsFromFd();
  installTakoGlobal();
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
    startServer: (
      handleRequest: (request: Request) => Promise<Response>,
    ) => number | void | Promise<number | void>,
  ): Promise<void> {
    if (!parsed.main) {
      console.error("Usage: <runtime> entrypoint <main> [--instance <id>] [--version <ver>]");
      process.exit(1);
    }

    let userFetch: FetchFunction;
    let userReady: (() => void | Promise<void>) | null = null;
    try {
      const module = await import(parsed.main);
      const defaultExport = module.default;
      if (typeof defaultExport === "function") {
        const readyable = defaultExport as ReadyableFetchHandler;
        userFetch = readyable;
        if (typeof readyable.ready === "function") {
          userReady = () => readyable.ready?.();
        }
      } else if (
        defaultExport &&
        typeof defaultExport === "object" &&
        typeof defaultExport.fetch === "function"
      ) {
        userFetch = defaultExport.fetch as FetchFunction;
        if (typeof defaultExport.ready === "function") {
          userReady = () => defaultExport.ready();
        }
      } else {
        throw new Error("App must export a default fetch function or { fetch } object.");
      }
    } catch (err) {
      console.error(`Failed to import app from ${parsed.main}:`, err);
      process.exit(1);
    }

    if (userReady) {
      try {
        await userReady();
      } catch (err) {
        console.error(`Failed to initialize app readiness from ${parsed.main}:`, err);
        process.exit(1);
      }
    }

    const env: Record<string, string> = {};
    for (const [key, value] of Object.entries(process.env)) {
      if (value !== undefined && key !== "TAKO_INTERNAL_TOKEN") {
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

    const actualPort = await startServer(handleRequest);
    currentStatus = "healthy";
    if (actualPort != null) {
      signalReadyPort(actualPort);
    }
  }

  return { run, host, port, setDraining };
}
