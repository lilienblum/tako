import type {
  ChannelAuthorizeInput,
  ChannelAuthorizeResponse,
  ChannelConnectOptions,
  ChannelConnection,
  ChannelDefinition,
  ChannelDefinitionTransport,
  ChannelMessage,
  ChannelPublishInput,
  ChannelPublishOptions,
  ChannelSubscribeOptions,
  ChannelSubscription,
} from "./types";

export const TAKO_CHANNELS_BASE_PATH = "/channels";
const DEFAULT_CHANNEL_REPLAY_WINDOW_MS = 24 * 60 * 60 * 1000;
const DEFAULT_CHANNEL_INACTIVITY_TTL_MS = 0;
const DEFAULT_CHANNEL_KEEPALIVE_INTERVAL_MS = 25 * 1000;
const DEFAULT_CHANNEL_MAX_CONNECTION_LIFETIME_MS = 2 * 60 * 60 * 1000;

interface ChannelDefinitionEntry {
  definition: ChannelDefinition;
  index: number;
  pattern: string;
}

function isExactPattern(pattern: string): boolean {
  return !pattern.includes("*");
}

function patternMatches(pattern: string, channel: string): boolean {
  if (pattern === "*") {
    return true;
  }
  if (isExactPattern(pattern)) {
    return pattern === channel;
  }
  if (pattern.endsWith("*")) {
    return channel.startsWith(pattern.slice(0, -1));
  }
  return false;
}

function patternSpecificity(pattern: string): number {
  return pattern.replace(/\*/g, "").length;
}

function normalizeBaseUrl(baseUrl?: string): URL {
  if (baseUrl) {
    return new URL(baseUrl);
  }
  if (typeof globalThis.location?.origin === "string" && globalThis.location.origin.length > 0) {
    return new URL(globalThis.location.origin);
  }
  throw new Error("Channel operations require a baseUrl outside the browser.");
}

function channelBaseUrl(channel: string, baseUrl?: string): URL {
  const url = normalizeBaseUrl(baseUrl);
  url.pathname = `${TAKO_CHANNELS_BASE_PATH}/${encodeURIComponent(channel)}`;
  url.search = "";
  return url;
}

function withQuery(url: URL, key: string, value?: string | number): URL {
  if (value !== undefined) {
    url.searchParams.set(key, String(value));
  }
  return url;
}

function toWebSocketUrl(url: URL): string {
  const ws = new URL(url.toString());
  ws.protocol = ws.protocol === "https:" ? "wss:" : "ws:";
  return ws.toString();
}

function requestHeaders(headers?: Record<string, string>): HeadersInit | undefined {
  if (!headers) {
    return undefined;
  }
  return headers;
}

function buildFetchInit(
  base: Omit<RequestInit, "headers" | "signal">,
  options: { headers?: HeadersInit; signal?: AbortSignal },
): RequestInit {
  const init: RequestInit = { ...base };
  if (options.headers !== undefined) {
    init.headers = options.headers;
  }
  if (options.signal !== undefined) {
    init.signal = options.signal;
  }
  return init;
}

function fetchInitOptions(headers?: HeadersInit, signal?: AbortSignal) {
  const options: { headers?: HeadersInit; signal?: AbortSignal } = {};
  if (headers !== undefined) {
    options.headers = headers;
  }
  if (signal !== undefined) {
    options.signal = signal;
  }
  return options;
}

function defaultEventSourceFactory(url: string): unknown {
  const ctor = globalThis.EventSource;
  if (!ctor) {
    throw new Error("EventSource is not available in this runtime.");
  }
  return new ctor(url);
}

function defaultWebSocketFactory(url: string): unknown {
  const ctor = globalThis.WebSocket;
  if (!ctor) {
    throw new Error("WebSocket is not available in this runtime.");
  }
  return new ctor(url);
}

function closeRaw(raw: unknown, code?: number, reason?: string): void {
  if (typeof raw !== "object" || raw === null) {
    return;
  }
  const maybeClose = (raw as { close?: (code?: number, reason?: string) => void }).close;
  if (typeof maybeClose === "function") {
    maybeClose.call(raw, code, reason);
  }
}

function sendRaw(raw: unknown, data: unknown): void {
  if (typeof raw !== "object" || raw === null) {
    throw new Error("Channel connection does not support send().");
  }
  const maybeSend = (raw as { send?: (data: unknown) => void }).send;
  if (typeof maybeSend !== "function") {
    throw new Error("Channel connection does not support send().");
  }
  let payload = data;
  if (
    data !== null &&
    typeof data === "object" &&
    !(data instanceof ArrayBuffer) &&
    !ArrayBuffer.isView(data) &&
    !(typeof Blob !== "undefined" && data instanceof Blob)
  ) {
    payload = JSON.stringify(data);
  }
  maybeSend.call(raw, payload);
}

export class Channel {
  readonly name: string;
  readonly transport: ChannelDefinitionTransport | undefined;

  constructor(name: string, transport?: ChannelDefinitionTransport) {
    this.name = name;
    this.transport = transport;
  }

  async publish<T = unknown>(
    message: ChannelPublishInput<T>,
    options: ChannelPublishOptions = {},
  ): Promise<ChannelMessage<T>> {
    const url = channelBaseUrl(this.name, options.baseUrl);
    url.pathname = `${url.pathname}/messages`;
    const response = await fetch(url.toString(), {
      ...buildFetchInit(
        {
          method: "POST",
          body: JSON.stringify(message),
        },
        {
          ...fetchInitOptions(
            {
              "Content-Type": "application/json",
              ...options.headers,
            },
            options.signal,
          ),
        },
      ),
    });

    if (!response.ok) {
      throw new Error(`Channel publish failed with status ${response.status}.`);
    }

    return (await response.json()) as ChannelMessage<T>;
  }

  subscribe(options: ChannelSubscribeOptions = {}): ChannelSubscription {
    const url = channelBaseUrl(this.name, options.baseUrl);
    const factory = options.eventSourceFactory ?? defaultEventSourceFactory;
    const init: { headers?: Record<string, string>; lastEventId?: string } = {};
    if (options.headers !== undefined) {
      init.headers = options.headers;
    }
    if (options.lastEventId !== undefined) {
      init.lastEventId = options.lastEventId;
    }
    const raw = factory(url.toString(), init);
    return {
      transport: "sse",
      raw,
      close() {
        closeRaw(raw);
      },
    };
  }

  connect(options: ChannelConnectOptions = {}): ChannelConnection {
    if (this.transport !== "ws") {
      throw new Error("Channel does not enable WebSocket transport.");
    }

    const url = channelBaseUrl(this.name, options.baseUrl);
    withQuery(url, "last_message_id", options.lastMessageId);

    const factory = options.webSocketFactory ?? defaultWebSocketFactory;
    const raw = factory(toWebSocketUrl(url));
    return {
      transport: "ws",
      raw,
      close(code?: number, reason?: string) {
        closeRaw(raw, code, reason);
      },
      send(data: unknown) {
        sendRaw(raw, data);
      },
    };
  }
}

export class ChannelRegistry {
  private definitions: ChannelDefinitionEntry[] = [];
  private nextIndex = 0;

  create(name: string, definition?: ChannelDefinition): Channel {
    if (definition) {
      this.define(name, definition);
      return new Channel(name, definition.transport);
    }
    return new Channel(name);
  }

  define(pattern: string, definition: ChannelDefinition): void {
    this.definitions.push({
      definition,
      index: this.nextIndex++,
      pattern,
    });
  }

  clear(): void {
    this.definitions = [];
    this.nextIndex = 0;
  }

  async authorize(input: ChannelAuthorizeInput): Promise<ChannelAuthorizeResponse> {
    const matched = this.resolveDefinition(input.channel);
    if (!matched) {
      return { ok: false };
    }

    const request = new Request(
      input.request.url,
      buildFetchInit(
        {
          method: input.request.method ?? "GET",
        },
        {
          ...fetchInitOptions(requestHeaders(flattenHeaders(input.request.headers))),
        },
      ),
    );
    const verdict = await matched.definition.auth(request, {
      channel: input.channel,
      operation: input.operation,
      pattern: matched.pattern,
    });

    const config = definitionLifecycleConfig(matched.definition);
    if (verdict === false) {
      return { ok: false };
    }
    if (verdict === true) {
      return { ok: true, ...config };
    }
    return verdict.subject === undefined
      ? { ok: true, ...config }
      : { ok: true, ...config, subject: verdict.subject };
  }

  resolveDefinition(channel: string): ChannelDefinitionEntry | null {
    return (
      this.definitions
        .filter((entry) => patternMatches(entry.pattern, channel))
        .sort((left, right) => {
          const exactWeight =
            Number(isExactPattern(right.pattern)) - Number(isExactPattern(left.pattern));
          if (exactWeight !== 0) {
            return exactWeight;
          }
          const specificity = patternSpecificity(right.pattern) - patternSpecificity(left.pattern);
          if (specificity !== 0) {
            return specificity;
          }
          return left.index - right.index;
        })[0] ?? null
    );
  }
}

function definitionLifecycleConfig(definition: ChannelDefinition) {
  const config: Omit<ChannelAuthorizeResponse, "ok" | "subject"> = {
    replayWindowMs: definition.replayWindowMs ?? DEFAULT_CHANNEL_REPLAY_WINDOW_MS,
    inactivityTtlMs: definition.inactivityTtlMs ?? DEFAULT_CHANNEL_INACTIVITY_TTL_MS,
    keepaliveIntervalMs: definition.keepaliveIntervalMs ?? DEFAULT_CHANNEL_KEEPALIVE_INTERVAL_MS,
    maxConnectionLifetimeMs:
      definition.maxConnectionLifetimeMs ?? DEFAULT_CHANNEL_MAX_CONNECTION_LIFETIME_MS,
  } as ChannelAuthorizeResponse;
  if (definition.transport === "ws") {
    config.transport = "ws";
  }
  return config;
}

function flattenHeaders(
  headers?: Record<string, string | string[]>,
): Record<string, string> | undefined {
  if (!headers) {
    return undefined;
  }
  const flattened: Record<string, string> = {};
  for (const [key, value] of Object.entries(headers)) {
    flattened[key] = Array.isArray(value) ? value.join(", ") : value;
  }
  return flattened;
}
