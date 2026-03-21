import { describe, expect, test } from "bun:test";
import { handleTakoEndpoint } from "../src/endpoints";
import { loadSecrets } from "../src/secrets";
import type { TakoStatus } from "../src/types";

describe("handleTakoEndpoint", () => {
  const mockStatus: TakoStatus = {
    status: "healthy",
    app: "test-app",
    version: "abc123",
    instance_id: "1",
    pid: 12345,
    uptime_seconds: 3600,
  };

  test("returns null for non-internal host even on /status", async () => {
    const request = new Request("http://localhost/status");
    const response = await handleTakoEndpoint(request, mockStatus);
    expect(response).toBeNull();
  });

  test("returns null for non-internal host paths", async () => {
    const request = new Request("http://localhost/api/users");
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
      const request = new Request("http://tako/status");
      const response = await handleTakoEndpoint(request, mockStatus);

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
      const request = new Request("http://tako/status");
      const response = await handleTakoEndpoint(request, unhealthyStatus);

      const body = await response!.json();
      expect(body.status).toBe("draining");
    });
    test("returns status for internal host with explicit port", async () => {
      const request = new Request("http://tako:3000/status");
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);
    });
  });

  describe("internal host /secrets", () => {
    test("accepts POST with JSON body and injects secrets", async () => {
      const secrets = { API_KEY: "secret123", DB_URL: "postgres://db" };
      const request = new Request("http://tako/secrets", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(secrets),
      });
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(200);

      const body = await response!.json();
      expect(body.status).toBe("ok");

      // Verify secrets are now accessible via the proxy
      const loaded = loadSecrets();
      expect(loaded.API_KEY).toBe("secret123");
      expect(loaded.DB_URL).toBe("postgres://db");
    });

    test("returns 405 for GET", async () => {
      const request = new Request("http://tako/secrets");
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(405);
    });

    test("returns 400 for invalid JSON", async () => {
      const request = new Request("http://tako/secrets", {
        method: "POST",
        body: "not json",
      });
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(400);
    });
  });

  describe("internal host unknown paths", () => {
    test("returns 404 for unknown paths on internal host", async () => {
      const request = new Request("http://tako/unknown");
      const response = await handleTakoEndpoint(request, mockStatus);

      expect(response).not.toBeNull();
      expect(response!.status).toBe(404);

      const body = await response!.json();
      expect(body.error).toBe("Not found");
    });
  });
});
