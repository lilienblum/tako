import { beforeEach, describe, expect, test } from "bun:test";
import {
  TAKO_INTERNAL_CHANNELS_AUTHORIZE_PATH,
  TAKO_INTERNAL_TOKEN_ENV,
  TAKO_INTERNAL_TOKEN_HEADER,
  handleTakoEndpoint,
} from "../src/endpoints";
import type { TakoStatus } from "../src/types";
import { Tako } from "../src/tako";

describe("handleTakoEndpoint", () => {
  process.env[TAKO_INTERNAL_TOKEN_ENV] = "test-token";

  beforeEach(() => {
    Tako.channels.clear();
  });

  const mockStatus: TakoStatus = {
    status: "healthy",
    app: "test-app",
    version: "abc123",
    instance_id: "1",
    pid: 12345,
    uptime_seconds: 3600,
  };

  test("returns null for non-internal host even on /status", async () => {
    const request = new Request("http://example.com/status");
    const response = await handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  test("returns null for non-internal host paths", async () => {
    const request = new Request("http://example.com/api/users");
    const response = await handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  test("returns null for root path on non-internal host", async () => {
    const request = new Request("http://example.com/");
    const response = await handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  describe("internal host /status", () => {
    test("returns status JSON", async () => {
      const request = new Request("http://tako.internal/status", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
      expect(response!.headers.get("Content-Type")).toBe("application/json");
      expect(response!.headers.get(TAKO_INTERNAL_TOKEN_HEADER)).toBe("test-token");

      const body = await response!.json();
      expect(body).toEqual(mockStatus);
    });

    test("returns current status value", async () => {
      const unhealthyStatus: TakoStatus = {
        ...mockStatus,
        status: "draining",
      };
      const request = new Request("http://tako.internal/status", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = await handleTakoEndpoint(request, unhealthyStatus);

      const body = await response!.json();
      expect(body.status).toBe("draining");
    });

    test("returns 403 without the internal token header", async () => {
      const request = new Request("http://tako.internal/status");
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(403);
    });

    test("returns status for internal host with explicit port", async () => {
      const request = new Request("http://tako.internal:3000/status", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
    });

    test("returns status for loopback host with valid token", async () => {
      const request = new Request("http://127.0.0.1:3000/status", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
    });
  });

  describe("internal host unknown paths", () => {
    test("returns 404 for unknown paths on internal host", async () => {
      const request = new Request("http://tako.internal/unknown", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(404);

      const body = await response!.json();
      expect(body.error).toBe("Not found");
    });
  });

  describe("internal host channel auth", () => {
    test("authorizes a matching channel definition", async () => {
      Tako.channels.define("chat:*", {
        auth(request, ctx) {
          expect(request.headers.get("authorization")).toBe("Bearer test");
          expect(ctx.channel).toBe("chat:room-123");
          expect(ctx.operation).toBe("subscribe");
          return { subject: "user-123" };
        },
      });

      const request = new Request(`http://tako.internal${TAKO_INTERNAL_CHANNELS_AUTHORIZE_PATH}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          [TAKO_INTERNAL_TOKEN_HEADER]: "test-token",
        },
        body: JSON.stringify({
          channel: "chat:room-123",
          operation: "subscribe",
          request: {
            url: "https://app.example.com/chat/room-123",
            method: "GET",
            headers: {
              authorization: "Bearer test",
            },
          },
        }),
      });

      const response = await handleTakoEndpoint(request, mockStatus);
      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
      expect(await response!.json()).toEqual({
        ok: true,
        replayWindowMs: 86_400_000,
        inactivityTtlMs: 0,
        keepaliveIntervalMs: 25_000,
        maxConnectionLifetimeMs: 7_200_000,
        subject: "user-123",
      });
    });

    test("returns 403 when channel auth denies access", async () => {
      Tako.channels.define("chat:*", {
        auth() {
          return false;
        },
      });

      const request = new Request(`http://tako.internal${TAKO_INTERNAL_CHANNELS_AUTHORIZE_PATH}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          [TAKO_INTERNAL_TOKEN_HEADER]: "test-token",
        },
        body: JSON.stringify({
          channel: "chat:room-123",
          operation: "subscribe",
          request: {
            url: "https://app.example.com/chat/room-123",
          },
        }),
      });

      const response = await handleTakoEndpoint(request, mockStatus);
      expect(response).not.toBeNull();
      expect(response!.status).toBe(403);
      expect(await response!.json()).toEqual({
        error: "Forbidden",
        ok: false,
      });
    });

    test("returns 404 when no channel definition matches", async () => {
      const request = new Request(`http://tako.internal${TAKO_INTERNAL_CHANNELS_AUTHORIZE_PATH}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          [TAKO_INTERNAL_TOKEN_HEADER]: "test-token",
        },
        body: JSON.stringify({
          channel: "chat:room-123",
          operation: "publish",
          request: {
            url: "https://app.example.com/chat/room-123",
          },
        }),
      });

      const response = await handleTakoEndpoint(request, mockStatus);
      expect(response).not.toBeNull();
      expect(response!.status).toBe(404);
      expect(await response!.json()).toEqual({
        error: "Channel not defined",
        ok: false,
      });
    });

    test("returns channel lifecycle config in authorize responses", async () => {
      Tako.channels.define("chat:*", {
        auth() {
          return { subject: "user-123" };
        },
        replayWindowMs: 86_400_000,
        inactivityTtlMs: 0,
        keepaliveIntervalMs: 25_000,
        maxConnectionLifetimeMs: 7_200_000,
        transport: "ws",
      });

      const request = new Request(`http://tako.internal${TAKO_INTERNAL_CHANNELS_AUTHORIZE_PATH}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          [TAKO_INTERNAL_TOKEN_HEADER]: "test-token",
        },
        body: JSON.stringify({
          channel: "chat:room-123",
          operation: "subscribe",
          request: {
            url: "https://app.example.com/chat/room-123",
          },
        }),
      });

      const response = await handleTakoEndpoint(request, mockStatus);
      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
      expect(await response!.json()).toEqual({
        ok: true,
        subject: "user-123",
        replayWindowMs: 86_400_000,
        inactivityTtlMs: 0,
        keepaliveIntervalMs: 25_000,
        maxConnectionLifetimeMs: 7_200_000,
        transport: "ws",
      });
    });
  });
});
