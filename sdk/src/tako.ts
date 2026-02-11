/**
 * Tako SDK Main Class
 *
 * Provides optional features for Tako apps.
 */

import type { TakoOptions } from "./types";

/**
 * Tako SDK class for optional app features
 *
 * @example
 * ```typescript
 * import { Tako } from 'tako.sh';
 *
 * const tako = new Tako({
 *   onConfigReload: (secrets) => {
 *     // Reconnect to database with new credentials
 *     database.reconnect(secrets.DATABASE_URL);
 *   }
 * });
 *
 * export default {
 *   fetch(request, env) {
 *     return new Response("Hello!");
 *   }
 * };
 * ```
 */
export class Tako {
  private static instance: Tako | null = null;
  private options: TakoOptions;

  constructor(options: TakoOptions = {}) {
    this.options = options;

    // Store as singleton for the wrapper to access
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
   * Register a config reload handler
   */
  onConfigReload(handler: (secrets: Record<string, string>) => void | Promise<void>): this {
    this.options.onConfigReload = handler;
    return this;
  }

  /**
   * Get Tako environment info
   */
  static getEnv(): {
    version: string;
    instanceId: number;
  } {
    return {
      version: process.env.TAKO_VERSION || "unknown",
      instanceId: parseInt(process.env.TAKO_INSTANCE || "0", 10),
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
