/**
 * Tako SDK Types
 */

/**
 * Standard web fetch handler interface
 * Compatible with Cloudflare Workers, Deno Deploy, Bun, etc.
 */
export type FetchFunction = (
  request: Request,
  env: Record<string, string>,
) => Response | Promise<Response>;

export type FetchHandler = FetchFunction;

export interface ReadyableFetchHandler extends FetchFunction {
  ready?: () => void | Promise<void>;
}

export type ChannelDefinitionTransport = "ws";
export type ChannelLiveTransport = "sse" | "ws";
export type ChannelOperation = "subscribe" | "publish" | "connect";

export interface ChannelAuthRequest {
  url: string;
  method?: string;
  headers?: Record<string, string | string[]>;
}

export interface ChannelAuthContext {
  channel: string;
  operation: ChannelOperation;
  pattern: string;
}

export interface ChannelGrant {
  subject?: string;
}

export type ChannelAuthResult = boolean | ChannelGrant | Promise<boolean | ChannelGrant>;

export interface ChannelLifecycleConfig {
  /** @defaultValue 86_400_000 (24 h) */
  replayWindowMs?: number;
  /** @defaultValue 0 (no inactivity eviction) */
  inactivityTtlMs?: number;
  /** @defaultValue 25_000 (25 s) */
  keepaliveIntervalMs?: number;
  /** @defaultValue 7_200_000 (2 h) */
  maxConnectionLifetimeMs?: number;
}

export interface ChannelDefinition extends ChannelLifecycleConfig {
  auth: (request: Request, context: ChannelAuthContext) => ChannelAuthResult;
  transport?: ChannelDefinitionTransport;
}

export interface ChannelAuthorizeInput {
  channel: string;
  operation: ChannelOperation;
  request: ChannelAuthRequest;
}

export interface ChannelAuthorizeResponse extends ChannelGrant, ChannelLifecycleConfig {
  ok: boolean;
  transport?: ChannelDefinitionTransport;
}

export interface ChannelPublishInput<T = unknown> {
  type: string;
  data: T;
}

export interface ChannelMessage<T = unknown> extends ChannelPublishInput<T> {
  id: string;
  channel: string;
}

export interface ChannelRequestOptions {
  baseUrl?: string;
  headers?: Record<string, string>;
  signal?: AbortSignal;
}

export interface EventSourceFactoryInit {
  headers?: Record<string, string>;
  lastEventId?: string;
}

export interface ChannelSubscribeOptions {
  baseUrl?: string;
  headers?: Record<string, string>;
  lastEventId?: string;
  eventSourceFactory?: (url: string, init?: EventSourceFactoryInit) => unknown;
}

export interface ChannelConnectOptions {
  baseUrl?: string;
  headers?: Record<string, string>;
  lastMessageId?: string;
  webSocketFactory?: (url: string) => unknown;
}

export interface ChannelPublishOptions extends ChannelRequestOptions {}

export interface ChannelSubscription {
  transport: "sse";
  raw: unknown;
  close: () => void;
}

export interface ChannelConnection {
  transport: "ws";
  raw: unknown;
  close: (code?: number, reason?: string) => void;
  send: (data: unknown) => void;
}

/**
 * Tako status response
 */
export interface TakoStatus {
  status: "healthy" | "starting" | "draining" | "unhealthy";
  app: string;
  version: string;
  instance_id: string;
  pid: number;
  uptime_seconds: number;
}
