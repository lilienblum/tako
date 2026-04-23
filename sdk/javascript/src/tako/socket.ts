/**
 * Shared Tako internal unix-socket RPC client.
 *
 * Server-side SDK code (app fetch handlers, workflow bodies, cron ticks)
 * reaches `tako-server` via a single unix socket. Workflow RPCs and
 * server-side channel `.publish()` both land here — no HTTPS, no auth, same
 * trust boundary as the hosting process.
 *
 * Env vars set by the server when spawning an instance or worker:
 *   TAKO_INTERNAL_SOCKET — path to the shared unix socket
 *   TAKO_APP_NAME        — app name used on every command payload
 */

import { createConnection } from "node:net";
import { setChannelSocketPublisher } from "../channels";
import { createLogger } from "../logger";
import type { ChannelMessage, ChannelPublishInput } from "../types";

export const INTERNAL_SOCKET_ENV = "TAKO_INTERNAL_SOCKET";
export const APP_NAME_ENV = "TAKO_APP_NAME";

export { TakoError, type TakoErrorCode } from "./error";
import { TakoError, type TakoErrorCode } from "./error";

interface RpcResponse {
  status: "ok" | "error";
  data?: unknown;
  message?: string;
}

const logger = createLogger("sdk.rpc");

/**
 * Log the raw failure and return a sanitized `TakoError`. Callers throw the
 * returned value; the original error stays on `.cause` for local debugging
 * but never flows to an end user via `.message`. The stable `code` field
 * lets app code branch by failure class.
 */
function wrapSocketError(code: TakoErrorCode, cause: unknown): TakoError {
  logger.error("rpc failed", { code, error: cause });
  return new TakoError(code, "Internal Server Error", { cause });
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

/**
 * Validate the Tako runtime env contract: `TAKO_INTERNAL_SOCKET` and
 * `TAKO_APP_NAME` must be set together or not at all.
 *
 * Called once at SDK init so a misconfigured spawn (one var set, the other
 * missing) crashes the process on boot instead of hiding until the first
 * workflow `.enqueue()` or channel `.publish()`. Both spawners
 * (`tako-server`, `tako-dev-server`) always set the pair, so a half-set
 * state is a platform bug worth failing loud.
 */
export function assertInternalSocketEnvConsistency(): void {
  const envObj = typeof process !== "undefined" ? process.env : undefined;
  if (!envObj) return;
  const hasSocket = Boolean(envObj[INTERNAL_SOCKET_ENV]);
  const hasApp = Boolean(envObj[APP_NAME_ENV]);
  if (hasSocket === hasApp) return;
  const missing = hasSocket ? APP_NAME_ENV : INTERNAL_SOCKET_ENV;
  const present = hasSocket ? INTERNAL_SOCKET_ENV : APP_NAME_ENV;
  throw new Error(
    `Tako SDK: ${present} is set but ${missing} is missing. ` +
      `Both env vars must be set together (or neither — when running ` +
      `outside a Tako-managed process). This usually means the spawner ` +
      `forgot to inject the full Tako runtime contract.`,
  );
}

/**
 * Install a channel publisher that routes `Channel.publish()` calls through
 * the Tako internal socket when `TAKO_INTERNAL_SOCKET` + `TAKO_APP_NAME` are
 * set. Called from the server and worker bootstraps so app/workflow code can
 * publish without an HTTPS round-trip back through the proxy.
 *
 * Returns `true` when a publisher was installed, `false` when the env is
 * missing (e.g. running outside a Tako-managed process).
 */
export function installChannelSocketPublisherFromEnv(): boolean {
  const env = internalSocketFromEnv();
  if (!env) return false;
  const { socketPath, app } = env;
  setChannelSocketPublisher(async <T>(channel: string, message: ChannelPublishInput<T>) => {
    const result = await callInternal(socketPath, {
      command: "channel_publish",
      app,
      channel,
      payload: message,
    });
    return result as ChannelMessage<T>;
  });
  return true;
}

/** Send a single JSONL command and resolve to `data` (or throw on error). */
export async function callInternal(socketPath: string, cmd: unknown): Promise<unknown> {
  const resp = await roundTrip(socketPath, cmd);
  if (resp.status === "error") {
    logger.error("rpc rejected", { code: "TAKO_RPC_ERROR", message: resp.message });
    throw new TakoError("TAKO_RPC_ERROR", "Internal Server Error", {
      cause: resp.message ? new Error(resp.message) : undefined,
    });
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

    socket.once("error", (err) => settle(() => reject(wrapSocketError("TAKO_UNAVAILABLE", err))));
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
        settle(() => reject(wrapSocketError("TAKO_PROTOCOL", err)));
      }
    });
    socket.once("end", () => {
      settle(() =>
        reject(wrapSocketError("TAKO_PROTOCOL", new Error("socket closed without response"))),
      );
    });
    socket.setTimeout(30_000, () => {
      settle(() => reject(wrapSocketError("TAKO_TIMEOUT", new Error("rpc timed out"))));
    });
  });
}
