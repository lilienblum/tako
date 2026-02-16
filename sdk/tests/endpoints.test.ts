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

  test("returns null for non-internal host even on /status", () => {
    const request = new Request("http://localhost/status");
    const response = handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  test("returns null for non-internal host paths", () => {
    const request = new Request("http://localhost/api/users");
    const response = handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  test("returns null for root path on non-internal host", () => {
    const request = new Request("http://example.com/");
    const response = handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  describe("internal host /status", () => {
    test("returns status JSON", async () => {
      const request = new Request("http://tako.internal/status");
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
      const request = new Request("http://tako.internal/status");
      const response = handleTakoEndpoint(request, unhealthyStatus);

      const body = await response!.json();
      expect(body.status).toBe("draining");
    });
    test("returns status for internal host with explicit port", async () => {
      const request = new Request("http://tako.internal:3000/status");
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
    });
  });

  describe("internal host unknown paths", () => {
    test("returns 404 for unknown paths on internal host", async () => {
      const request = new Request("http://tako.internal/unknown");
      const response = handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(404);

      const body = await response!.json();
      expect(body.error).toBe("Not found");
    });
  });
});
