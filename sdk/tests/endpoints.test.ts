import { describe, expect, test } from "bun:test";
import { handleTakoEndpoint } from "../src/endpoints";
import type { TakoStatus } from "../src/types";

describe("handleTakoEndpoint", () => {
  const mockStatus: TakoStatus = {
    status: "healthy",
    app: "test-app",
    version: "abc123",
    instance_id: 1,
    pid: 12345,
    uptime_seconds: 3600,
  };

  test("returns null for non-tako paths", () => {
    const request = new Request("http://localhost/api/users");
    const response = handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  test("returns null for root path", () => {
    const request = new Request("http://localhost/");
    const response = handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  describe("/_tako/status", () => {
    test("returns status JSON", async () => {
      const request = new Request("http://localhost/_tako/status");
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
      expect(response!.headers.get("Content-Type")).toBe("application/json");

      const body = await response!.json();
      expect(body).toEqual(mockStatus);
    });

    test("returns current status value", async () => {
      const unhealthyStatus: TakoStatus = {
        ...mockStatus,
        status: "draining",
      };
      const request = new Request("http://localhost/_tako/status");
      const response = handleTakoEndpoint(request, unhealthyStatus);

      const body = await response!.json();
      expect(body.status).toBe("draining");
    });
  });

  describe("/_tako/health", () => {
    test("returns 200 when healthy", async () => {
      const request = new Request("http://localhost/_tako/health");
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);

      const body = await response!.json();
      expect(body.status).toBe("ok");
    });

    test("returns 503 when not healthy", async () => {
      const unhealthyStatus: TakoStatus = {
        ...mockStatus,
        status: "draining",
      };
      const request = new Request("http://localhost/_tako/health");
      const response = handleTakoEndpoint(request, unhealthyStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(503);

      const body = await response!.json();
      expect(body.status).toBe("draining");
    });

    test("returns 503 when starting", async () => {
      const startingStatus: TakoStatus = {
        ...mockStatus,
        status: "starting",
      };
      const request = new Request("http://localhost/_tako/health");
      const response = handleTakoEndpoint(request, startingStatus);

      expect(response!.status).toBe(503);
    });

    test("returns 503 when unhealthy", async () => {
      const unhealthyStatus: TakoStatus = {
        ...mockStatus,
        status: "unhealthy",
      };
      const request = new Request("http://localhost/_tako/health");
      const response = handleTakoEndpoint(request, unhealthyStatus);

      expect(response!.status).toBe(503);
    });
  });

  describe("unknown /_tako/ paths", () => {
    test("returns 404 for unknown paths", async () => {
      const request = new Request("http://localhost/_tako/unknown");
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(404);

      const body = await response!.json();
      expect(body.error).toBe("Not found");
    });

    test("returns 404 for /_tako/ without subpath", async () => {
      const request = new Request("http://localhost/_tako/");
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response!.status).toBe(404);
    });
  });
});
