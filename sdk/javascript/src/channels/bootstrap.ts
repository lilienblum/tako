import { join } from "node:path";
import { discoverChannels } from "./discovery";
import { ChannelRegistry } from "../channels";

const CHANNELS_DIRNAME = "channels";

export interface ChannelBootstrapOptions {
  appDir: string;
}

export interface ChannelBootstrapResult {
  registry: ChannelRegistry;
  channelCount: number;
}

/**
 * Discover channels from `<appDir>/channels/` and return a fresh
 * {@link ChannelRegistry} populated with them. Callers hold the registry
 * for the life of the process and pass it to the endpoints handler when
 * authorizing or dispatching.
 */
export async function bootstrapChannels(
  opts: ChannelBootstrapOptions,
): Promise<ChannelBootstrapResult> {
  const dir = join(opts.appDir, CHANNELS_DIRNAME);
  const found = await discoverChannels(dir);
  const registry = new ChannelRegistry();
  for (const { name, definition } of found) {
    registry.register(name, definition);
  }
  return { registry, channelCount: found.length };
}
