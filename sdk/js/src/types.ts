/**
 * Tako SDK Types
 */

/**
 * Standard web fetch handler interface
 * Compatible with Cloudflare Workers, Deno Deploy, Bun, etc.
 */
export type FetchFunction = (
  request: Request,
  env: Record<string, string>,
) => Response | Promise<Response>;

export type FetchHandler = FetchFunction;

/**
 * Options for Tako SDK
 */
export type TakoOptions = Record<string, never>;

/**
 * Tako status response
 */
export interface TakoStatus {
  status: "healthy" | "starting" | "draining" | "unhealthy";
  app: string;
  version: string;
  instance_id: number;
  pid: number;
  uptime_seconds: number;
}
