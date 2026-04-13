import { describe, expect, test } from "bun:test";
import type {
  ChannelConnection,
  ChannelDefinition,
  ChannelOperation,
  ChannelSubscription,
  FetchHandler,
  TakoOptions,
  TakoStatus,
} from "../src/types";

describe("Types", () => {
  describe("FetchHandler", () => {
    test("accepts default fetch function handler", () => {
      const handler: FetchHandler = (_request: Request, _env: Record<string, string>) => {
        return new Response("Hello");
      };
      expect(typeof handler).toBe("function");
    });

    test("handler is callable", () => {
      const handler: FetchHandler = (_request: Request, _env: Record<string, string>) =>
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
        instance_id: "1",
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
          instance_id: "1",
          pid: 12345,
          uptime_seconds: 100,
        };
        expect(status.status).toBe(s);
      }
    });
  });

  describe("channel types", () => {
    test("accepts channel operations", () => {
      const operations: ChannelOperation[] = ["subscribe", "publish", "connect"];
      expect(operations).toContain("publish");
    });

    test("accepts channel definitions", () => {
      const definition: ChannelDefinition = {
        auth() {
          return true;
        },
        transport: "ws",
        replayWindowMs: 86_400_000,
        keepaliveIntervalMs: 25_000,
      };

      expect(definition.transport).toBe("ws");
      expect(definition.replayWindowMs).toBe(86_400_000);
      expect(definition.keepaliveIntervalMs).toBe(25_000);
    });

    test("distinguishes read-only subscriptions from send-capable connections", () => {
      const subscription: ChannelSubscription = {
        transport: "sse",
        raw: {},
        close() {},
      };
      const connection: ChannelConnection = {
        transport: "ws",
        raw: {},
        close() {},
        send() {},
      };

      expect(subscription.transport).toBe("sse");
      expect(connection.transport).toBe("ws");
    });
  });
});
