/**
 * Browser-safe runtime surface consumed by the generated `tako.gen.ts`.
 *
 * Only exposes symbols whose module graph is free of `node:*` imports, so
 * Vite / Bun / Webpack can bundle a `tako.gen.ts` into a client chunk
 * without tripping their browser-externalization stubs for `node:fs`,
 * `node:net`, etc. Server-adapter surface (`handleTakoEndpoint`,
 * `initServerRuntime`) lives on `tako.sh/internal` — do not re-export it
 * from here.
 */

export { createLogger, Logger } from "./logger";
export type { Logger as LoggerType } from "./logger";
export { loadSecrets } from "./tako/secrets";
