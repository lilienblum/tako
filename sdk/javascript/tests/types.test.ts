import { describe, expect, test } from "bun:test";
import type {
  ChannelOperation,
  ChannelSocket,
  ChannelSubscription,
  FetchHandler,
  TakoStatus,
} from "../src/types";
import { defineChannel } from "../src/channels/define";
import type { ChannelDefinition } from "../src/channels/define";

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

    test("accepts channel definitions built with defineChannel", () => {
      const definition: ChannelDefinition = defineChannel<{ msg: { text: string } }>(
        "chat/:roomId",
        {
          auth: async () => true,
          handler: { msg: async (d) => d },
          replayWindowMs: 86_400_000,
          keepaliveIntervalMs: 25_000,
        },
      );

      expect(definition.pattern).toBe("chat/:roomId");
      expect(definition.handler).toBeDefined();
      expect(definition.replayWindowMs).toBe(86_400_000);
      expect(definition.keepaliveIntervalMs).toBe(25_000);
    });

    test("distinguishes read-only subscriptions from send-capable sockets", () => {
      const subscription: ChannelSubscription = {
        transport: "sse",
        raw: {},
        close() {},
      };
      const socket: ChannelSocket = {
        transport: "ws",
        raw: {},
        close() {},
        send() {},
      };

      expect(subscription.transport).toBe("sse");
      expect(socket.transport).toBe("ws");
    });
  });
});
