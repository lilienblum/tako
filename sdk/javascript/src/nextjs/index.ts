import { fileURLToPath } from "node:url";

import { createNextjsAdapter } from "./adapter";
import type { NextConfigShape } from "./types";

export { createNextjsAdapter } from "./adapter";
export { createNextjsFetchHandler, shutdownManagedNextjsServers } from "./fetch-handler";
export type {
  NextAdapterContext,
  NextAdapterShape,
  NextBuildCompleteContext,
  NextConfigShape,
  NextjsBuildManifest,
  NextjsFetchHandlerOptions,
} from "./types";

export function withTako<T extends NextConfigShape>(config: T): T & NextConfigShape {
  return {
    ...config,
    output: "standalone",
    adapterPath: fileURLToPath(import.meta.url),
    allowedDevOrigins: [...(config.allowedDevOrigins ?? []), "*.test", "*.tako.test"],
  };
}

export default createNextjsAdapter();
