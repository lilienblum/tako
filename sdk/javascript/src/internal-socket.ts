/**
 * Shared Tako internal-socket RPC client.
 *
 * Server-side SDK code (app fetch handlers, workflow bodies, cron ticks)
 * reaches `tako-server` via a single unix socket. Workflow RPCs and
 * `Tako.channels.publish()` both land here — no HTTPS, no auth, same
 * trust boundary as the hosting process.
 *
 * Env vars set by the server when spawning an instance or worker:
 *   TAKO_INTERNAL_SOCKET — path to the shared unix socket
 *   TAKO_APP_NAME        — app name used on every command payload
 */

import { createConnection } from "node:net";

export const INTERNAL_SOCKET_ENV = "TAKO_INTERNAL_SOCKET";
export const APP_NAME_ENV = "TAKO_APP_NAME";

export class InternalSocketError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "InternalSocketError";
  }
}

interface RpcResponse {
  status: "ok" | "error";
  data?: unknown;
  message?: string;
}

/**
 * Look up the `(socketPath, appName)` pair from env. Returns `null` when
 * either var is missing — callers decide whether to fall back (HTTPS for
 * channels) or throw (workflow RPC).
 */
export function internalSocketFromEnv(): { socketPath: string; app: string } | null {
  const envObj = typeof process !== "undefined" ? process.env : undefined;
  if (!envObj) return null;
  const socketPath = envObj[INTERNAL_SOCKET_ENV];
  const app = envObj[APP_NAME_ENV];
  if (!socketPath || !app) return null;
  return { socketPath, app };
}

/** Send a single JSONL command and resolve to `data` (or throw on error). */
export async function callInternal(socketPath: string, cmd: unknown): Promise<unknown> {
  const resp = await roundTrip(socketPath, cmd);
  if (resp.status === "error") {
    throw new InternalSocketError(resp.message ?? "rpc failed");
  }
  return resp.data ?? null;
}

function roundTrip(socketPath: string, cmd: unknown): Promise<RpcResponse> {
  return new Promise<RpcResponse>((resolve, reject) => {
    const socket = createConnection(socketPath);
    let buf = "";
    let settled = false;

    const settle = (fn: () => void): void => {
      if (settled) return;
      settled = true;
      socket.removeAllListeners();
      socket.destroy();
      fn();
    };

    socket.once("error", (err) => settle(() => reject(err)));
    socket.once("connect", () => {
      socket.write(`${JSON.stringify(cmd)}\n`);
    });
    socket.on("data", (chunk: Buffer) => {
      buf += chunk.toString("utf8");
      const nl = buf.indexOf("\n");
      if (nl === -1) return;
      const line = buf.slice(0, nl);
      try {
        settle(() => resolve(JSON.parse(line) as RpcResponse));
      } catch (err) {
        settle(() => reject(new InternalSocketError(`invalid JSON from server: ${String(err)}`)));
      }
    });
    socket.once("end", () => {
      settle(() => reject(new InternalSocketError("socket closed without response")));
    });
    socket.setTimeout(30_000, () => {
      settle(() => reject(new InternalSocketError("rpc timed out")));
    });
  });
}
