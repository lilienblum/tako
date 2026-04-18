import { join } from "node:path";
import { discoverChannels } from "./discovery";
import type { ChannelRegistry } from "../channels";
import { buildChannelAccessor } from "./accessor";

const CHANNELS_DIRNAME = "channels";

const ACCESSOR_INSTALLED = Symbol.for("tako.channels.accessor.installed");

export interface ChannelBootstrapOptions {
  appDir: string;
  registry: ChannelRegistry;
}

export interface ChannelBootstrapResult {
  channelCount: number;
}

export async function bootstrapChannels(
  opts: ChannelBootstrapOptions,
): Promise<ChannelBootstrapResult> {
  const dir = join(opts.appDir, CHANNELS_DIRNAME);
  const found = await discoverChannels(dir);
  opts.registry.clear();
  for (const { name, definition } of found) {
    opts.registry.register(name, definition);
  }
  attachAccessor(opts.registry);
  return { channelCount: found.length };
}

function attachAccessor(registry: ChannelRegistry): void {
  const bag = buildChannelAccessor(registry);
  const target = registry as unknown as Record<string | symbol, unknown>;
  if (target[ACCESSOR_INSTALLED]) {
    for (const key of Object.keys(target[ACCESSOR_INSTALLED] as object)) {
      delete target[key];
    }
  }
  for (const [key, value] of Object.entries(bag)) {
    target[key] = value;
  }
  target[ACCESSOR_INSTALLED] = bag;
}
