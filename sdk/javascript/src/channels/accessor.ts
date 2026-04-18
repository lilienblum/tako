import { Channel } from "../channels";
import type { ChannelRegistry } from "../channels";
import type { ChannelDefinition } from "./define";

export interface UnparameterizedHandle {
  send: (type: string, data: unknown) => Promise<unknown>;
  subscribe: Channel["subscribe"];
  connect?: Channel["connect"];
}

export type ParameterizedAccessor = (params: Record<string, string>) => UnparameterizedHandle;

export type ChannelAccessorEntry = UnparameterizedHandle | ParameterizedAccessor;

export function buildChannelAccessor(
  registry: ChannelRegistry,
): Record<string, ChannelAccessorEntry> {
  const out: Record<string, ChannelAccessorEntry> = {};
  for (const { name, definition } of registry.all) {
    const isWs = definition.handler !== undefined;
    if (definition.compiled.paramNames.length === 0) {
      out[name] = makeHandle(definition.pattern, isWs);
    } else {
      out[name] = (params: Record<string, string>) => {
        const channelName = expandPattern(definition.pattern, params);
        return makeHandle(channelName, isWs);
      };
    }
  }
  return out;
}

function makeHandle(channelName: string, isWs: boolean): UnparameterizedHandle {
  const channel = new Channel(channelName, isWs ? "ws" : undefined);
  const handle: UnparameterizedHandle = {
    send: (type: string, data: unknown) => channel.publish({ type, data }),
    subscribe: channel.subscribe.bind(channel),
  };
  if (isWs) {
    handle.connect = channel.connect.bind(channel);
  }
  return handle;
}

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

/** Helper type for codegen: extract the message map from a channel definition. */
export type InferChannelMessages<T> = T extends ChannelDefinition<infer M> ? M : never;

/** Helper type for codegen: extract the pattern string literal from a channel definition. */
export type InferChannelPattern<T> = T extends ChannelDefinition & { readonly pattern: infer P }
  ? P
  : never;

/** Helper type for codegen: is the channel definition's handler populated (WS)? */
export type InferChannelHasHandler<T> = T extends ChannelDefinition & {
  readonly handler: object;
}
  ? true
  : false;
