/**
 * Tako SDK Main Class
 *
 * Provides optional features for Tako apps.
 */

import type { TakoOptions } from "./types";
import { loadSecrets } from "./secrets";

/**
 * Tako SDK class for optional app features
 *
 * @example
 * ```typescript
 * import { Tako } from 'tako.sh';
 *
 * export default function fetch(request: Request, env: Record<string, string>) {
 *   return new Response("Hello!");
 * }
 * ```
 */
export class Tako {
  private static instance: Tako | null = null;
  private options: TakoOptions;

  /**
   * Secrets loaded from the Tako runtime. Access individual secrets
   * as properties: `Tako.secrets.DATABASE_URL`
   *
   * The secrets object resists serialization — `JSON.stringify(Tako.secrets)`
   * and `console.log(Tako.secrets)` return "[REDACTED]".
   */
  static readonly secrets: Record<string, string> = loadSecrets();

  constructor(options: TakoOptions = {}) {
    this.options = options;

    // Store as singleton for the entrypoint to access
    Tako.instance = this;
  }

  /**
   * Get the singleton instance
   */
  static getInstance(): Tako | null {
    return Tako.instance;
  }

  /**
   * Get the options
   */
  getOptions(): TakoOptions {
    return this.options;
  }

  /**
   * Get Tako environment info
   */
  static getEnv(): {
    version: string;
    instanceId: string;
  } {
    return {
      version: process.env.TAKO_VERSION || "unknown",
      instanceId: process.env.TAKO_INSTANCE || "unknown",
    };
  }

  /**
   * Check if running in Tako environment
   */
  static isRunningInTako(): boolean {
    // If Tako injected any of its runtime metadata, treat it as "running under Tako".
    return !!(process.env.TAKO_APP_SOCKET || process.env.TAKO_BUILD || process.env.TAKO_VERSION);
  }
}
