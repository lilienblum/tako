/**
 * Tako SDK surface.
 *
 * The `Tako` export is what users import from `tako.sh` at module-load
 * time to register channel handlers and define channel policies. Env
 * vars (`env`, `port`, etc.) are exposed on the ambient `globalThis.Tako`
 * installed by `installTakoGlobal`; see the `declare global` block below.
 */

import { ChannelRegistry } from "./channels";
import { loadSecrets } from "./tako/secrets";
import { workflowsEngine } from "./workflows/engine";
import type { EnqueueOptions } from "./workflows/engine";
import type { RunId, WorkflowConfig } from "./workflows/types";
import type { WorkflowHandler } from "./workflows/worker";

function parsePort(raw: string | undefined): number | undefined {
  if (!raw) return undefined;
  const n = Number(raw);
  return Number.isFinite(n) ? n : undefined;
}

/**
 * Module-load-time accessors. Use `Tako.channels` to define and register
 * channel handlers, `Tako.secrets` for typed secret reads.
 *
 * @example
 * ```typescript
 * import { Tako } from "tako.sh";
 *
 * Tako.channels.define("chat:*", { auth: async () => ({ allow: true }) });
 * ```
 */
/**
 * User-facing workflows surface. `enqueue` schedules a run; `register` is
 * the imperative alternative to the `workflows/` directory convention.
 */
const workflows = {
  enqueue(name: string, payload: unknown, opts?: EnqueueOptions): Promise<RunId> {
    return workflowsEngine.enqueue(name, payload, opts);
  },
  register(name: string, handler: WorkflowHandler, config?: WorkflowConfig): void {
    workflowsEngine.register(name, handler, config);
  },
  signal(eventName: string, payload?: unknown): Promise<number> {
    return workflowsEngine.signal(eventName, payload);
  },
} as const;

export const Tako = {
  channels: new ChannelRegistry(),
  secrets: loadSecrets(),
  workflows,
} as const;

type RuntimeState = {
  env: string | undefined;
  port: number | undefined;
  host: string | undefined;
  build: string;
  dataDir: string | undefined;
  appDir: string;
};

const runtimeState: RuntimeState = {
  env: undefined,
  port: undefined,
  host: undefined,
  build: "unknown",
  dataDir: undefined,
  appDir: process.cwd(),
};

const globalTako = Object.freeze(
  Object.defineProperties(
    {},
    {
      secrets: {
        value: Tako.secrets,
        writable: false,
        configurable: false,
        enumerable: false,
      },
      channels: {
        value: Tako.channels,
        writable: false,
        configurable: false,
        enumerable: false,
      },
      workflows: {
        value: Tako.workflows,
        writable: false,
        configurable: false,
        enumerable: false,
      },
      env: { get: () => runtimeState.env, configurable: false, enumerable: false },
      isDev: {
        get: () => runtimeState.env === "development",
        configurable: false,
        enumerable: false,
      },
      isProd: {
        get: () => runtimeState.env === "production",
        configurable: false,
        enumerable: false,
      },
      port: { get: () => runtimeState.port, configurable: false, enumerable: false },
      host: { get: () => runtimeState.host, configurable: false, enumerable: false },
      build: { get: () => runtimeState.build, configurable: false, enumerable: false },
      dataDir: { get: () => runtimeState.dataDir, configurable: false, enumerable: false },
      appDir: { get: () => runtimeState.appDir, configurable: false, enumerable: false },
    },
  ),
);

function refreshRuntimeState(): void {
  runtimeState.env = process.env["ENV"];
  runtimeState.port = parsePort(process.env["PORT"]);
  runtimeState.host = process.env["HOST"];
  runtimeState.build = process.env["TAKO_BUILD"] || "unknown";
  runtimeState.dataDir = process.env["TAKO_DATA_DIR"];
  runtimeState.appDir = process.cwd();
}

declare global {
  /**
   * Project-specific secret names are augmented onto this interface by the
   * generated `tako.d.ts`. The empty placeholder here lets the global
   * `Tako.secrets` type resolve even before the generator has run.
   */
  // eslint-disable-next-line @typescript-eslint/no-empty-object-type
  interface TakoSecrets {}

  /**
   * Global Tako runtime surface — installed by the Tako entrypoint before
   * your app's module is imported. Accessible anywhere in your app without
   * an import statement.
   */
  const Tako: {
    /** Current environment. */
    readonly env?: string;
    /** `true` when `env === "development"`. */
    readonly isDev: boolean;
    /** `true` when `env === "production"`. */
    readonly isProd: boolean;
    /** Port Tako assigned to this app instance. */
    readonly port?: number;
    /** Host/address Tako bound this app instance to. */
    readonly host?: string;
    /** Build identifier injected by Tako at deploy time. */
    readonly build: string;
    /** Persistent app-owned data directory. Writes survive restarts. */
    readonly dataDir?: string;
    /** Directory the app is running from (equivalent to `process.cwd()`). */
    readonly appDir: string;
    /** Typed secret accessor. */
    readonly secrets: TakoSecrets;
    /** Durable workflow engine. */
    readonly workflows: {
      /** Enqueue a run of the named workflow. */
      enqueue(name: string, payload: unknown, opts?: EnqueueOptions): Promise<RunId>;
      /** Imperative workflow registration (alternative to the workflows/ dir). */
      register(name: string, handler: WorkflowHandler, config?: WorkflowConfig): void;
      /** Deliver an event payload to every parked `step.waitFor(name)`. */
      signal(eventName: string, payload?: unknown): Promise<number>;
    };
  };
}

/**
 * Attach a frozen `Tako` object to `globalThis` so user modules — including
 * transitively imported ones — can reach secrets, channels, and build info
 * without importing from `tako.sh`.
 *
 * Repeated calls refresh the runtime snapshot, so tests and multi-entry
 * setups do not depend on first-install wins behavior.
 */
export function installTakoGlobal(): void {
  refreshRuntimeState();

  if (Object.getOwnPropertyDescriptor(globalThis, "Tako")) return;

  Object.defineProperty(globalThis, "Tako", {
    value: globalTako,
    writable: false,
    configurable: false,
    enumerable: false,
  });
}
