/**
 * Tako SDK Main Class
 *
 * Provides optional features for Tako apps.
 */

import { ChannelRegistry } from "./channels";
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
  static readonly channels = new ChannelRegistry();
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
   * Get the Tako build version (set via TAKO_BUILD env var in deploy manifest).
   */
  static get build(): string {
    return process.env["TAKO_BUILD"] || "unknown";
  }

  /**
   * Check if running in Tako environment.
   */
  static isRunningInTako(): boolean {
    return !!process.env["TAKO_BUILD"];
  }
}
