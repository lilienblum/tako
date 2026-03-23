import { describe, expect, test } from "bun:test";
import {
  TAKO_INTERNAL_TOKEN_ENV,
  TAKO_INTERNAL_TOKEN_HEADER,
  handleTakoEndpoint,
} from "../src/endpoints";
import type { TakoStatus } from "../src/types";

describe("handleTakoEndpoint", () => {
  process.env[TAKO_INTERNAL_TOKEN_ENV] = "test-token";

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
    const response = handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  test("returns null for non-internal host paths", async () => {
    const request = new Request("http://example.com/api/users");
    const response = handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  test("returns null for root path on non-internal host", async () => {
    const request = new Request("http://example.com/");
    const response = handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  describe("internal host /status", () => {
    test("returns status JSON", async () => {
      const request = new Request("http://tako/status", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = handleTakoEndpoint(request, mockStatus);

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
      const request = new Request("http://tako/status", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = handleTakoEndpoint(request, unhealthyStatus);

      const body = await response!.json();
      expect(body.status).toBe("draining");
    });

    test("returns 403 without the internal token header", async () => {
      const request = new Request("http://tako/status");
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(403);
    });

    test("returns status for internal host with explicit port", async () => {
      const request = new Request("http://tako:3000/status", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
    });

    test("returns status for loopback host with valid token", async () => {
      const request = new Request("http://127.0.0.1:3000/status", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
    });
  });

  describe("internal host unknown paths", () => {
    test("returns 404 for unknown paths on internal host", async () => {
      const request = new Request("http://tako/unknown", {
        headers: { [TAKO_INTERNAL_TOKEN_HEADER]: "test-token" },
      });
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(404);

      const body = await response!.json();
      expect(body.error).toBe("Not found");
    });
  });
});
