/**
 * tako.sh Node.js Adapter (internal)
 *
 * Used by the tako-node entrypoint binary. Not a public API.
 */

import type { TakoOptions, TakoStatus } from "../types";
import {
  TAKO_INTERNAL_HOST,
  TAKO_INTERNAL_STATUS_PATH,
  TAKO_INTERNAL_TOKEN_ENV,
  TAKO_INTERNAL_TOKEN_HEADER,
} from "../endpoints";

// Re-export core classes
export { Tako } from "../tako";
export type { TakoOptions, TakoStatus, FetchHandler } from "../types";

// Environment variables set by tako
const TAKO_VERSION = process.env["TAKO_VERSION"] || "unknown";
const TAKO_INSTANCE = process.env["TAKO_INSTANCE"] || "unknown";
const TAKO_INTERNAL_TOKEN = process.env[TAKO_INTERNAL_TOKEN_ENV] || "";

const startedAt = Date.now();
let status: TakoStatus["status"] = "starting";

/**
 * Get current Tako status
 */
export function getStatus(): TakoStatus {
  return {
    status,
    app: "app",
    version: TAKO_VERSION,
    instance_id: TAKO_INSTANCE,
    pid: process.pid,
    uptime_seconds: Math.floor((Date.now() - startedAt) / 1000),
  };
}

/**
 * Set the current status
 */
export function setStatus(newStatus: TakoStatus["status"]): void {
  status = newStatus;
}

/**
 * Express/Connect-style middleware for Tako internal endpoints
 *
 * Handles:
 * - GET /status on Host `tako` - Returns app status
 */
export function createMiddleware(): (
  req: {
    url?: string;
    method?: string;
    headers?: { host?: string | string[] };
  },
  res: {
    writeHead: (status: number, headers: Record<string, string>) => void;
    end: (body: string) => void;
  },
  next: () => void,
) => void {
  const normalizeHost = (value: string | string[] | undefined): string => {
    const candidate = Array.isArray(value) ? value[0] : value;
    return (candidate ?? "").trim().toLowerCase().split(":")[0] ?? "";
  };

  const requestPathname = (value: string): string => {
    try {
      return new URL(value, "http://localhost").pathname;
    } catch {
      return "/";
    }
  };

  return (req, res, next) => {
    const url = req.url || "/";
    const method = req.method || "GET";
    const host = normalizeHost(req.headers?.host);
    const pathname = requestPathname(url);
    const token = (req.headers as Record<string, string | string[] | undefined> | undefined)?.[
      TAKO_INTERNAL_TOKEN_HEADER
    ];
    const normalizedToken = Array.isArray(token) ? token[0] : token;

    if (host !== TAKO_INTERNAL_HOST) {
      next();
      return;
    }
    if (!TAKO_INTERNAL_TOKEN || normalizedToken !== TAKO_INTERNAL_TOKEN) {
      res.writeHead(403, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "Forbidden" }));
      return;
    }

    if (pathname === TAKO_INTERNAL_STATUS_PATH && method === "GET") {
      const statusData = getStatus();
      res.writeHead(200, {
        "Content-Type": "application/json",
        [TAKO_INTERNAL_TOKEN_HEADER]: TAKO_INTERNAL_TOKEN,
      });
      res.end(JSON.stringify(statusData));
      return;
    }

    res.writeHead(404, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "Not found" }));
  };
}

/**
 * Initialize Tako for Node.js
 *
 * Call this at app startup to:
 * - Set status to healthy
 * - Setup graceful shutdown handlers
 */
export function init(_options?: TakoOptions): void {
  status = "healthy";

  // Handle graceful shutdown
  process.on("SIGTERM", () => {
    console.log("[tako.sh] Received SIGTERM, draining...");
    status = "draining";
  });

  process.on("SIGINT", () => {
    console.log("[tako.sh] Received SIGINT, shutting down...");
    process.exit(0);
  });

  console.log("[tako.sh] Node.js adapter initialized");
}
