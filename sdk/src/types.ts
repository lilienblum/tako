/**
 * Tako SDK Types
 */

/**
 * Standard web fetch handler interface
 * Compatible with Cloudflare Workers, Deno Deploy, Bun, etc.
 */
export interface FetchHandler {
  fetch(request: Request, env: Record<string, string>): Response | Promise<Response>;
}

/**
 * Options for Tako SDK
 */
export interface TakoOptions {
  /**
   * Called when secrets/config are reloaded at runtime.
   * Use this to reconnect to databases with new credentials, etc.
   */
  onConfigReload?: (secrets: Record<string, string>) => void | Promise<void>;
}

/**
 * Tako status response
 */
export interface TakoStatus {
  status: "healthy" | "starting" | "draining" | "unhealthy";
  app: string;
  version: string;
  instance_id: number;
  pid: number;
  uptime_seconds: number;
}

/**
 * Messages sent from app to tako-server
 */
export type AppToServerMessage =
  | ReadyMessage
  | HeartbeatMessage
  | ShutdownAckMessage;

export interface ReadyMessage {
  type: "ready";
  app: string;
  version: string;
  instance_id: number;
  pid: number;
  socket_path: string;
  timestamp: string;
}

export interface HeartbeatMessage {
  type: "heartbeat";
  app: string;
  instance_id: number;
  pid: number;
  timestamp: string;
}

export interface ShutdownAckMessage {
  type: "shutdown_ack";
  app: string;
  instance_id: number;
  pid: number;
  drained: boolean;
  timestamp: string;
}

/**
 * Messages sent from tako-server to app
 */
export type ServerToAppMessage = ShutdownMessage | ReloadConfigMessage;

export interface ShutdownMessage {
  type: "shutdown";
  reason: "deploy" | "restart" | "scale_down" | "stop";
  drain_timeout_seconds: number;
}

export interface ReloadConfigMessage {
  type: "reload_config";
  secrets: Record<string, string>;
}

/**
 * Server connection response
 */
export interface ServerAck {
  status: "ack" | "error";
  message: string;
}
