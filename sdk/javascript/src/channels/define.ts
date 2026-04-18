import type { CompiledPattern } from "./pattern";
import { compilePattern } from "./pattern";

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
> extends ChannelLifecycleConfig {
  readonly type: typeof CHANNEL_SYMBOL;
  readonly pattern: string;
  readonly compiled: CompiledPattern;
  readonly auth: ResolvedAuth<Record<string, string>>;
  readonly handler?: ChannelConfig<Messages, Record<string, string>>["handler"];
}

const ALLOW_ALL_AUTH = async () => true as const;

export function defineChannel<Messages = Record<string, unknown>>(
  pattern: string,
  config: ChannelConfig<Messages, Record<string, string>> = {},
): ChannelDefinition<Messages> {
  const compiled = compilePattern(pattern);
  return {
    type: CHANNEL_SYMBOL,
    pattern,
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
}

export function isChannelDefinition(value: unknown): value is ChannelDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    "type" in value &&
    (value as { type: unknown }).type === CHANNEL_SYMBOL
  );
}
