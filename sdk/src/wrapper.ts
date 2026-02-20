/**
 * Tako Runtime Wrapper
 *
 * This is the internal wrapper that Tako uses to run user apps.
 * It handles:
 * - Unix socket server creation
 * - Connection to tako-server
 * - Ready/heartbeat signals
 * - Internal status endpoint (Host: tako.internal, Path: /status)
 * - Graceful shutdown
 *
 * Users don't interact with this directly - it's used by tako dev and tako-server.
 */

import { ServerConnection } from "./connection";
import { handleTakoEndpoint } from "./endpoints";
import { resolveAppSocketPath } from "./socket-path";
import { Tako } from "./tako";
import type { FetchFunction, TakoStatus, TakoOptions } from "./types";
import { isAbsolute, resolve } from "node:path";
import { pathToFileURL } from "node:url";

// Environment variables set by tako
const TAKO_VERSION = process.env.TAKO_VERSION || "unknown";
const TAKO_INSTANCE = parseInt(process.env.TAKO_INSTANCE || "1", 10);
const TAKO_SOCKET = process.env.TAKO_SOCKET;
const TAKO_APP_SOCKET = process.env.TAKO_APP_SOCKET;
const appSocketPath = resolveAppSocketPath(TAKO_APP_SOCKET);

const DEFAULT_TAKO_SOCKET = "/var/run/tako/tako.sock";
const serverSocketPath = TAKO_SOCKET || DEFAULT_TAKO_SOCKET;

// Track startup time for uptime calculation
const startedAt = Date.now();

// Current status
let currentStatus: TakoStatus["status"] = "starting";

/**
 * Get current Tako status
 */
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

/**
 * Create the app server
 */
async function createAppServer(userFetch: FetchFunction, options: TakoOptions): Promise<void> {
  // Build environment object from process.env
  const env: Record<string, string> = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (value !== undefined) {
      env[key] = value;
    }
  }

  // Create request handler
  const handleRequest = async (request: Request): Promise<Response> => {
    // Check for Tako internal endpoints first
    const takoResponse = handleTakoEndpoint(request, getStatus());
    if (takoResponse) {
      return takoResponse;
    }

    // Pass through to user app
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

  // Start Unix socket server or TCP server depending on mode
  if (appSocketPath) {
    // Production mode: Unix socket
    console.log(`Starting Unix socket server at ${appSocketPath}`);

    const server = Bun.serve({
      unix: appSocketPath,
      fetch: handleRequest,
    });

    console.log(`Application listening on ${appSocketPath}`);
  } else {
    // Dev mode or fallback: TCP
    const port = parseInt(process.env.PORT || "3000", 10);

    const server = Bun.serve({
      port,
      fetch: handleRequest,
    });

    console.log(`Application listening on http://localhost:${port}`);
  }

  // Mark as healthy once server is listening
  currentStatus = "healthy";
}

/**
 * Connect to tako-server (production mode only)
 */
export async function connectToServer(options: TakoOptions): Promise<void> {
  if (!appSocketPath) {
    return;
  }

  console.log(`Connecting to tako-server at ${serverSocketPath}`);

  const connection = new ServerConnection(
    serverSocketPath,
    "app",
    TAKO_VERSION,
    TAKO_INSTANCE,
    appSocketPath,
    options,
  );

  try {
    const ack = await connection.connect();
    console.log(`Server acknowledged: ${ack.message}`);

    // Start heartbeat
    connection.startHeartbeat();

    // Handle process signals
    process.on("SIGTERM", () => {
      console.log("Received SIGTERM");
      currentStatus = "draining";
      // Connection will handle shutdown
    });

    process.on("SIGINT", () => {
      console.log("Received SIGINT");
      connection.close();
      process.exit(0);
    });
  } catch (err) {
    console.error("Failed to connect to server:", err);
    // Continue running anyway - server might be unavailable temporarily
  }
}

/**
 * Run the Tako wrapper
 *
 * @param userAppPath - Path to the user's app module
 */
export function resolveUserAppImportUrl(userAppPath: string): string {
  const absPath = isAbsolute(userAppPath) ? userAppPath : resolve(process.cwd(), userAppPath);
  return pathToFileURL(absPath).href;
}

export async function run(userAppPath: string): Promise<void> {
  console.log("Starting application");

  // Import user's app
  let userFetch: FetchFunction;
  try {
    const module = await import(resolveUserAppImportUrl(userAppPath));
    const defaultExport = module.default;

    if (typeof defaultExport === "function") {
      userFetch = defaultExport as FetchFunction;
    } else if (defaultExport && typeof defaultExport.fetch === "function") {
      userFetch = defaultExport.fetch.bind(defaultExport) as FetchFunction;
    } else {
      throw new Error(
        "App must export a default fetch(request, env) function (or legacy default object with fetch).",
      );
    }
  } catch (err) {
    console.error(`Failed to import app from ${userAppPath}:`, err);
    process.exit(1);
  }

  // Get options from Tako instance if it exists
  const takoInstance = Tako.getInstance();
  const options: TakoOptions = takoInstance?.getOptions() || {};

  // Create the app server
  await createAppServer(userFetch, options);

  // Connect to tako-server (production only)
  await connectToServer(options);

  console.log("Wrapper initialized successfully");
}

// If this is the main module, run with the app path from args
if (import.meta.main) {
  const appPath = process.argv[2];
  if (!appPath) {
    console.error("Usage: bun run wrapper.ts <app-path>");
    process.exit(1);
  }
  run(appPath);
}
