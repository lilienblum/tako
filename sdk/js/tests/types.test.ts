import { describe, expect, test } from "bun:test";
import type { FetchHandler, TakoOptions, TakoStatus } from "../src/types";

describe("Types", () => {
  describe("FetchHandler", () => {
    test("accepts default fetch function handler", () => {
      const handler: FetchHandler = (request: Request, env: Record<string, string>) => {
        return new Response("Hello");
      };
      expect(typeof handler).toBe("function");
    });

    test("handler is callable", () => {
      const handler: FetchHandler = (request: Request, env: Record<string, string>) =>
        new Response("Hello");
      expect(typeof handler).toBe("function");
    });
  });

  describe("TakoOptions", () => {
    test("accepts empty options", () => {
      const options: TakoOptions = {};
      expect(options).toEqual({});
    });
  });

  describe("TakoStatus", () => {
    test("accepts healthy status", () => {
      const status: TakoStatus = {
        status: "healthy",
        app: "my-app",
        version: "abc123",
        instance_id: 1,
        pid: 12345,
        uptime_seconds: 100,
      };
      expect(status.status).toBe("healthy");
    });

    test("accepts all status values", () => {
      const statuses: TakoStatus["status"][] = ["healthy", "starting", "draining", "unhealthy"];
      for (const s of statuses) {
        const status: TakoStatus = {
          status: s,
          app: "my-app",
          version: "abc123",
          instance_id: 1,
          pid: 12345,
          uptime_seconds: 100,
        };
        expect(status.status).toBe(s);
      }
    });
  });
});
