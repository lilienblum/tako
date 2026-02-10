import { describe, expect, test } from "bun:test";
import type {
  FetchHandler,
  TakoOptions,
  TakoStatus,
  AppToServerMessage,
  ServerToAppMessage,
  ReadyMessage,
  HeartbeatMessage,
  ShutdownAckMessage,
  ShutdownMessage,
  ReloadConfigMessage,
  ServerAck,
} from "../src/types";

describe("Types", () => {
  describe("FetchHandler", () => {
    test("accepts valid fetch handler", () => {
      const handler: FetchHandler = {
        fetch: (request: Request, env: Record<string, string>) => {
          return new Response("Hello");
        },
      };
      expect(handler.fetch).toBeDefined();
    });

    test("accepts async fetch handler", () => {
      const handler: FetchHandler = {
        fetch: async (request: Request, env: Record<string, string>) => {
          return new Response("Hello");
        },
      };
      expect(handler.fetch).toBeDefined();
    });
  });

  describe("TakoOptions", () => {
    test("accepts empty options", () => {
      const options: TakoOptions = {};
      expect(options).toEqual({});
    });

    test("accepts onConfigReload handler", () => {
      const options: TakoOptions = {
        onConfigReload: (secrets) => {
          console.log(secrets);
        },
      };
      expect(options.onConfigReload).toBeDefined();
    });

    test("accepts async onConfigReload handler", () => {
      const options: TakoOptions = {
        onConfigReload: async (secrets) => {
          await Promise.resolve();
        },
      };
      expect(options.onConfigReload).toBeDefined();
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
      const statuses: TakoStatus["status"][] = [
        "healthy",
        "starting",
        "draining",
        "unhealthy",
      ];
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

  describe("AppToServerMessage", () => {
    test("accepts ready message", () => {
      const msg: ReadyMessage = {
        type: "ready",
        app: "my-app",
        version: "abc123",
        instance_id: 1,
        pid: 12345,
        socket_path: "/var/run/tako-app-my-app-12345.sock",
        timestamp: new Date().toISOString(),
      };
      expect(msg.type).toBe("ready");
    });

    test("accepts heartbeat message", () => {
      const msg: HeartbeatMessage = {
        type: "heartbeat",
        app: "my-app",
        instance_id: 1,
        pid: 12345,
        timestamp: new Date().toISOString(),
      };
      expect(msg.type).toBe("heartbeat");
    });

    test("accepts shutdown_ack message", () => {
      const msg: ShutdownAckMessage = {
        type: "shutdown_ack",
        app: "my-app",
        instance_id: 1,
        pid: 12345,
        drained: true,
        timestamp: new Date().toISOString(),
      };
      expect(msg.type).toBe("shutdown_ack");
    });

    test("union type accepts all message types", () => {
      const messages: AppToServerMessage[] = [
        {
          type: "ready",
          app: "my-app",
          version: "abc123",
          instance_id: 1,
          pid: 12345,
          socket_path: "/tmp/sock",
          timestamp: new Date().toISOString(),
        },
        {
          type: "heartbeat",
          app: "my-app",
          instance_id: 1,
          pid: 12345,
          timestamp: new Date().toISOString(),
        },
        {
          type: "shutdown_ack",
          app: "my-app",
          instance_id: 1,
          pid: 12345,
          drained: true,
          timestamp: new Date().toISOString(),
        },
      ];
      expect(messages.length).toBe(3);
    });
  });

  describe("ServerToAppMessage", () => {
    test("accepts shutdown message", () => {
      const msg: ShutdownMessage = {
        type: "shutdown",
        reason: "deploy",
        drain_timeout_seconds: 30,
      };
      expect(msg.type).toBe("shutdown");
    });

    test("accepts all shutdown reasons", () => {
      const reasons: ShutdownMessage["reason"][] = [
        "deploy",
        "restart",
        "scale_down",
        "stop",
      ];
      for (const reason of reasons) {
        const msg: ShutdownMessage = {
          type: "shutdown",
          reason,
          drain_timeout_seconds: 30,
        };
        expect(msg.reason).toBe(reason);
      }
    });

    test("accepts reload_config message", () => {
      const msg: ReloadConfigMessage = {
        type: "reload_config",
        secrets: {
          DATABASE_URL: "postgres://...",
          API_KEY: "secret123",
        },
      };
      expect(msg.type).toBe("reload_config");
      expect(msg.secrets.DATABASE_URL).toBe("postgres://...");
    });

    test("union type accepts all message types", () => {
      const messages: ServerToAppMessage[] = [
        {
          type: "shutdown",
          reason: "deploy",
          drain_timeout_seconds: 30,
        },
        {
          type: "reload_config",
          secrets: { KEY: "value" },
        },
      ];
      expect(messages.length).toBe(2);
    });
  });

  describe("ServerAck", () => {
    test("accepts ack status", () => {
      const ack: ServerAck = {
        status: "ack",
        message: "Instance registered",
      };
      expect(ack.status).toBe("ack");
    });

    test("accepts error status", () => {
      const ack: ServerAck = {
        status: "error",
        message: "Registration failed",
      };
      expect(ack.status).toBe("error");
    });
  });
});
