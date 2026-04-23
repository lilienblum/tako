import type { CompiledPattern } from "./pattern";
import { compilePattern } from "./pattern";
import { Channel } from "../channels";
import type {
  ChannelConnectOptions,
  ChannelMessage,
  ChannelPublishOptions,
  ChannelSocket,
  ChannelSubscribeOptions,
  ChannelSubscription,
} from "../types";

export const CHANNEL_SYMBOL = Symbol("channel");

type ChannelAuthResult = boolean | { subject?: string };

export interface ChannelAuthContext<Params = Record<string, string>> {
  channel: string;
  operation: "subscribe" | "publish" | "connect";
  pattern: string;
  params: Params;
}

export interface ChannelHandlerContext<
  Params = Record<string, string>,
> extends ChannelAuthContext<Params> {
  subject?: string;
  publishedBy: "server" | "client";
}

export type MessageHandler<Data, Params> = (
  data: Data,
  ctx: ChannelHandlerContext<Params>,
) => Data | void | Promise<Data | void>;

export interface ChannelLifecycleConfig {
  replayWindowMs?: number;
  inactivityTtlMs?: number;
  keepaliveIntervalMs?: number;
  maxConnectionLifetimeMs?: number;
}

export interface ChannelConfig<Messages, Params> extends ChannelLifecycleConfig {
  /**
   * Auth callback. Return `false` to deny, `true` to allow anonymously, or a
   * {@link ChannelGrant} to allow and stamp the connection with a subject.
   * Omit to allow all access — use only for channels intended to be public.
   */
  auth?: (
    request: Request,
    ctx: ChannelAuthContext<Params>,
  ) => ChannelAuthResult | Promise<ChannelAuthResult>;
  handler?: { [T in keyof Messages]?: MessageHandler<Messages[T], Params> };
}

export type ResolvedAuth<Params> = (
  request: Request,
  ctx: ChannelAuthContext<Params>,
) => ChannelAuthResult | Promise<ChannelAuthResult>;

export interface ChannelDefinition<
  Messages = Record<string, unknown>,
  Pattern extends string = string,
> extends ChannelLifecycleConfig {
  readonly type: typeof CHANNEL_SYMBOL;
  readonly pattern: Pattern;
  readonly compiled: CompiledPattern;
  readonly auth: ResolvedAuth<Record<string, string>>;
  readonly handler?: ChannelConfig<Messages, Record<string, string>>["handler"];
}

/**
 * Extract `:param` names from a pattern string literal into a typed params
 * object: `"chat/:room/:msg"` → `{ room: string; msg: string }`;
 * `"status"` → `{}`.
 */
// eslint-disable-next-line @typescript-eslint/no-empty-object-type
export type ChannelPathParams<P extends string> = P extends `${string}:${infer Rest}`
  ? Rest extends `${infer Name}/${infer Tail}`
    ? { [K in Name]: string } & ChannelPathParams<Tail>
    : { [K in Rest]: string }
  : {};

type PatternHasParams<P extends string> = P extends `${string}:${string}` ? true : false;

/**
 * Typed handle returned by a channel export after parameter substitution
 * (or directly, for unparameterized channels). `publish` is constrained to
 * the channel's declared message map.
 */
export interface ChannelHandle<Messages> {
  /** Fully-resolved channel name (params substituted and URL-encoded). */
  readonly name: string;
  publish<T extends keyof Messages>(
    message: { type: T; data: Messages[T] },
    options?: ChannelPublishOptions,
  ): Promise<ChannelMessage<Messages[T]>>;
  subscribe(options?: ChannelSubscribeOptions): ChannelSubscription;
  connect?(options?: ChannelConnectOptions): ChannelSocket;
}

/**
 * Shared surface on every channel export regardless of parameterization:
 * the `definition` metadata used by server-side discovery/auth, and the
 * chainable `.$messageTypes<M>()` type-level narrower (runtime no-op).
 */
export interface ChannelExportMeta<Messages, Pattern extends string> {
  readonly definition: ChannelDefinition<Messages, Pattern>;
  /** Type-level narrower. Returns the same export typed with the given message map. */
  $messageTypes<NewMessages>(): ChannelExport<NewMessages, Pattern>;
}

/**
 * The default export from a `channels/<name>.ts` file. Callable with
 * params when the pattern has `:param` segments; a direct handle when it
 * doesn't. Either way, `.definition` and `.$messageTypes()` are always there.
 */
export type ChannelExport<
  Messages,
  Pattern extends string,
> = (PatternHasParams<Pattern> extends true
  ? (params: ChannelPathParams<Pattern>) => ChannelHandle<Messages>
  : ChannelHandle<Messages>) &
  ChannelExportMeta<Messages, Pattern>;

const ALLOW_ALL_AUTH = async () => true as const;

function expandPattern(pattern: string, params: Record<string, string>): string {
  return pattern
    .split("/")
    .map((seg) => {
      if (!seg.startsWith(":")) return seg;
      const name = seg.slice(1);
      const value = params[name];
      if (value === undefined || value === null || value === "") {
        throw new Error(`missing channel param '${name}'`);
      }
      return encodeURIComponent(value);
    })
    .join("/");
}

function makeHandle<M>(
  definition: ChannelDefinition<M, string>,
  resolvedName: string,
): ChannelHandle<M> {
  const isWs = definition.handler !== undefined;
  const channel = new Channel(resolvedName, isWs ? "ws" : undefined);
  const handle: ChannelHandle<M> = {
    name: resolvedName,
    publish: channel.publish.bind(channel) as ChannelHandle<M>["publish"],
    subscribe: channel.subscribe.bind(channel),
  };
  if (isWs) {
    handle.connect = channel.connect.bind(channel);
  }
  return handle;
}

function attachMeta<M, P extends string, T extends object>(
  target: T,
  definition: ChannelDefinition<M, P>,
): T & ChannelExportMeta<M, P> {
  Object.defineProperty(target, "definition", {
    value: definition,
    writable: false,
    enumerable: false,
    configurable: false,
  });
  Object.defineProperty(target, "$messageTypes", {
    value: function messageTypesNarrow<NewM>() {
      return this as unknown as ChannelExport<NewM, P>;
    },
    writable: false,
    enumerable: false,
    configurable: false,
  });
  return target as T & ChannelExportMeta<M, P>;
}

export function defineChannel<
  Messages = Record<string, unknown>,
  const Pattern extends string = string,
>(
  pattern: Pattern,
  config: ChannelConfig<Messages, Record<string, string>> = {},
): ChannelExport<Messages, Pattern> {
  const compiled = compilePattern(pattern);
  const definition: ChannelDefinition<Messages, Pattern> = {
    type: CHANNEL_SYMBOL,
    pattern: pattern as Pattern,
    compiled,
    auth: config.auth ?? ALLOW_ALL_AUTH,
    ...(config.handler !== undefined && { handler: config.handler }),
    ...(config.replayWindowMs !== undefined && { replayWindowMs: config.replayWindowMs }),
    ...(config.inactivityTtlMs !== undefined && { inactivityTtlMs: config.inactivityTtlMs }),
    ...(config.keepaliveIntervalMs !== undefined && {
      keepaliveIntervalMs: config.keepaliveIntervalMs,
    }),
    ...(config.maxConnectionLifetimeMs !== undefined && {
      maxConnectionLifetimeMs: config.maxConnectionLifetimeMs,
    }),
  };

  if (compiled.paramNames.length > 0) {
    const callable = (params: Record<string, string>) =>
      makeHandle(definition, expandPattern(definition.pattern, params));
    return attachMeta(callable, definition) as unknown as ChannelExport<Messages, Pattern>;
  }

  const handle = makeHandle(definition, definition.pattern);
  return attachMeta(handle, definition) as unknown as ChannelExport<Messages, Pattern>;
}

/** Narrow `value` to a `ChannelExport` produced by `defineChannel`. */
export function isChannelExport(value: unknown): value is ChannelExport<unknown, string> {
  return (
    value !== null &&
    (typeof value === "function" || typeof value === "object") &&
    "definition" in (value as object) &&
    isChannelDefinition((value as { definition: unknown }).definition)
  );
}

export function isChannelDefinition(value: unknown): value is ChannelDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    "type" in value &&
    (value as { type: unknown }).type === CHANNEL_SYMBOL
  );
}
