/**
 * tako.sh Node.js Adapter
 *
 * Provides Node.js-specific Tako functionality.
 * Works with Express, Fastify, or native http module.
 *
 * @example
 * ```typescript
 * import { Tako, createMiddleware } from 'tako.sh/node';
 * import express from 'express';
 *
 * const tako = new Tako({
 *   onConfigReload: (secrets) => {
 *     console.log('Config reloaded:', secrets);
 *   }
 * });
 *
 * const app = express();
 * app.use(createMiddleware());
 * app.get('/', (req, res) => res.send('Hello from Node!'));
 * app.listen(3000);
 * ```
 */

import { Tako } from "../tako";
import type { TakoOptions, TakoStatus } from "../types";
import { TAKO_INTERNAL_HOST, TAKO_INTERNAL_STATUS_PATH } from "../endpoints";

// Re-export core classes
export { Tako } from "../tako";
export type { TakoOptions, TakoStatus } from "../types";

// Environment variables set by tako
const TAKO_VERSION = process.env.TAKO_VERSION || "unknown";
const TAKO_INSTANCE = parseInt(process.env.TAKO_INSTANCE || "1", 10);

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
 * - GET /status on Host `tako.internal` - Returns app status
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
    return (candidate || "").trim().toLowerCase().split(":")[0];
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

    if (host !== TAKO_INTERNAL_HOST) {
      next();
      return;
    }

    if (pathname === TAKO_INTERNAL_STATUS_PATH && method === "GET") {
      const statusData = getStatus();
      res.writeHead(200, { "Content-Type": "application/json" });
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
export function init(options?: TakoOptions): void {
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
