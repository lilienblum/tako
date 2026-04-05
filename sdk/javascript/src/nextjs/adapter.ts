import { stageNextjsBuildOutput } from "./staging";
import type { NextAdapterShape } from "./types";

export function createNextjsAdapter(): NextAdapterShape {
  return {
    name: "tako-nextjs",
    modifyConfig(config) {
      return {
        ...config,
        output: "standalone",
      };
    },
    async onBuildComplete({ projectDir, distDir }) {
      await stageNextjsBuildOutput(projectDir, distDir);
    },
  };
}
