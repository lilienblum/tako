/**
 * tako.sh Node.js Adapter
 *
 * Provides Node.js-specific Tako functionality.
 * Works with Express, Fastify, or native http module.
 *
 * @example
 * ```typescript
 * import { createMiddleware } from 'tako.sh/node';
 * import express from 'express';
 *
 * const app = express();
 * app.use(createMiddleware());
 * app.get('/', (req, res) => res.send('Hello from Node!'));
 * app.listen(3000);
 * ```
 */

import type { TakoOptions, TakoStatus } from "../types";
import {
  TAKO_INTERNAL_HOST,
  TAKO_INTERNAL_STATUS_PATH,
  TAKO_INTERNAL_SECRETS_PATH,
} from "../endpoints";
import { injectSecrets } from "../secrets";

// Re-export core classes
export { Tako } from "../tako";
export type { TakoOptions, TakoStatus, FetchHandler } from "../types";

// Environment variables set by tako
const TAKO_VERSION = process.env["TAKO_VERSION"] || "unknown";
const TAKO_INSTANCE = process.env["TAKO_INSTANCE"] || "unknown";

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

    if (pathname === TAKO_INTERNAL_SECRETS_PATH && method === "POST") {
      let body = "";
      (req as any).on("data", (chunk: Buffer) => {
        body += chunk.toString();
      });
      (req as any).on("end", () => {
        try {
          const secrets = JSON.parse(body);
          injectSecrets(secrets);
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ status: "ok" }));
        } catch {
          res.writeHead(400, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ error: "Invalid JSON body" }));
        }
      });
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
