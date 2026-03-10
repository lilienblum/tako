/**
 * Creates a Tako entrypoint for any JS runtime.
 *
 * Each runtime-specific entrypoint (bun.ts, node.ts, deno.ts) calls this
 * and only provides the server binding layer.
 */

import { handleTakoEndpoint } from "./endpoints";
import { resolveAppSocketPath } from "./socket-path";
import { Tako } from "./tako";
import type { FetchFunction, TakoStatus } from "./types";
import { isAbsolute, resolve } from "node:path";
import { pathToFileURL } from "node:url";

export function resolveUserAppImportUrl(userAppPath: string): string {
  const absPath = isAbsolute(userAppPath) ? userAppPath : resolve(process.cwd(), userAppPath);
  return pathToFileURL(absPath).href;
}

export function createEntrypoint() {
  const TAKO_VERSION = process.env.TAKO_VERSION || "unknown";
  const TAKO_INSTANCE = process.env.TAKO_INSTANCE || "unknown";
  const appSocketPath = resolveAppSocketPath(process.env.TAKO_APP_SOCKET);
  const port = parseInt(process.env.PORT || "3000", 10);

  const startedAt = Date.now();
  let currentStatus: TakoStatus["status"] = "starting";

  function getStatus(): TakoStatus {
    return {
      status: currentStatus,
      app: "app",
      version: TAKO_VERSION,
      instance_id: TAKO_INSTANCE,
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
    const appPath = process.argv[2];
    if (!appPath) {
      console.error("Usage: <runtime> entrypoint.ts <app-path>");
      process.exit(1);
    }

    console.log("Starting application");

    let userFetch: FetchFunction;
    try {
      const module = await import(resolveUserAppImportUrl(appPath));
      const defaultExport = module.default;
      if (typeof defaultExport !== "function") {
        throw new Error("App must export a default fetch(request, env) function.");
      }
      userFetch = defaultExport as FetchFunction;
    } catch (err) {
      console.error(`Failed to import app from ${appPath}:`, err);
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
      const takoResponse = handleTakoEndpoint(request, getStatus());
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
