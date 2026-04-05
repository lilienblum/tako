import type { ChildProcess } from "node:child_process";

export type NextConfigShape = Record<string, unknown> & {
  adapterPath?: string;
  output?: string;
};

export interface NextAdapterContext {
  phase: string;
  nextVersion: string;
}

export interface NextBuildCompleteContext {
  routing: Record<string, unknown>;
  outputs: Record<string, unknown>;
  projectDir: string;
  repoRoot: string;
  distDir: string;
  config: NextConfigShape;
  nextVersion: string;
  buildId: string;
}

export interface NextAdapterShape {
  name: string;
  modifyConfig?: (
    config: NextConfigShape,
    ctx: NextAdapterContext,
  ) => Promise<NextConfigShape> | NextConfigShape;
  onBuildComplete?: (ctx: NextBuildCompleteContext) => Promise<void> | void;
}

export interface NextjsFetchHandlerOptions {
  hostname?: string;
  startupTimeoutMs?: number;
  argv?: string[];
  cwd?: string | URL;
  unstable_testing?: {
    ensureServer?: () => Promise<number>;
    fetchImplementation?: typeof fetch;
  };
}

export interface ManagedNextjsServer {
  child: ChildProcess | null;
  ready: Promise<number> | null;
  argv: string[];
  cwd: string;
  hostname: string;
  startupTimeoutMs: number;
}

export interface NextjsBuildManifest {
  distRoot: string;
  takoEntrypoint: string;
  standaloneDir: string;
  standaloneServer: string;
  staticDir: string;
  publicDir: string;
  standaloneStaticDir: string;
  standalonePublicDir: string;
}
