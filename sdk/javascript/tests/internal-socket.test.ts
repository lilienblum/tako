import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import type { Server } from "node:net";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  APP_NAME_ENV,
  assertInternalSocketEnvConsistency,
  callInternal,
  INTERNAL_SOCKET_ENV,
  internalSocketFromEnv,
  TakoError,
} from "../src/internal-socket";

function clearEnv(): void {
  delete process.env[INTERNAL_SOCKET_ENV];
  delete process.env[APP_NAME_ENV];
}

describe("internalSocketFromEnv", () => {
  beforeEach(clearEnv);
  afterEach(clearEnv);

  test("returns null when neither env var is set", () => {
    expect(internalSocketFromEnv()).toBeNull();
  });

  test("returns null when only socket is set", () => {
    process.env[INTERNAL_SOCKET_ENV] = "/tmp/tako.sock";
    expect(internalSocketFromEnv()).toBeNull();
  });

  test("returns null when only app is set", () => {
    process.env[APP_NAME_ENV] = "demo";
    expect(internalSocketFromEnv()).toBeNull();
  });

  test("returns the pair when both are set", () => {
    process.env[INTERNAL_SOCKET_ENV] = "/tmp/tako.sock";
    process.env[APP_NAME_ENV] = "demo";
    expect(internalSocketFromEnv()).toEqual({
      socketPath: "/tmp/tako.sock",
      app: "demo",
    });
  });
});

describe("assertInternalSocketEnvConsistency", () => {
  beforeEach(clearEnv);
  afterEach(clearEnv);

  test("passes when both env vars are set", () => {
    process.env[INTERNAL_SOCKET_ENV] = "/tmp/tako.sock";
    process.env[APP_NAME_ENV] = "demo";
    expect(() => {
      assertInternalSocketEnvConsistency();
    }).not.toThrow();
  });

  test("passes when neither env var is set (app running outside Tako)", () => {
    expect(() => {
      assertInternalSocketEnvConsistency();
    }).not.toThrow();
  });

  test("throws when only TAKO_INTERNAL_SOCKET is set — TAKO_APP_NAME missing means RPCs can't route", () => {
    process.env[INTERNAL_SOCKET_ENV] = "/tmp/tako.sock";
    expect(() => {
      assertInternalSocketEnvConsistency();
    }).toThrow(/TAKO_APP_NAME/);
  });

  test("throws when only TAKO_APP_NAME is set — missing socket means workflows/channels have nowhere to send", () => {
    process.env[APP_NAME_ENV] = "demo";
    expect(() => {
      assertInternalSocketEnvConsistency();
    }).toThrow(/TAKO_INTERNAL_SOCKET/);
  });
});

describe("callInternal error wrapping", () => {
  let dir: string;

  beforeEach(async () => {
    dir = await mkdtemp(join("/tmp", "tako-sock-err-"));
  });

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  test("maps a missing unix socket to TakoError TAKO_UNAVAILABLE without leaking the path", async () => {
    const missing = join(dir, "nonexistent.sock");
    let caught: unknown;
    try {
      await callInternal(missing, { command: "noop" });
    } catch (err) {
      caught = err;
    }
    expect(caught).toBeInstanceOf(TakoError);
    const err = caught as TakoError;
    expect(err.code).toBe("TAKO_UNAVAILABLE");
    expect(err.message).not.toContain(missing);
    expect(err.message).not.toContain("ENOENT");
    expect(err.message).not.toContain("connect");
    // Message is brand-neutral — apps can surface it directly without
    // leaking "Tako" to end users.
    expect(err.message).not.toContain("Tako");
    expect(err.message).toBe("Internal Server Error");
    // Original error is preserved for operators on .cause.
    expect(err.cause).toBeDefined();
  });

  test("maps a server error response to TakoError TAKO_RPC_ERROR with the server message", async () => {
    const sock = join(dir, "srv.sock");
    const server = await new Promise<Server>((resolve, reject) => {
      const s = createServer((socket) => {
        socket.on("data", () => {
          socket.write(`${JSON.stringify({ status: "error", message: "unknown workflow 'x'" })}\n`);
        });
      });
      s.once("error", reject);
      s.listen(sock, () => resolve(s));
    });
    try {
      let caught: unknown;
      try {
        await callInternal(sock, { command: "enqueue_run" });
      } catch (err) {
        caught = err;
      }
      expect(caught).toBeInstanceOf(TakoError);
      const err = caught as TakoError;
      expect(err.code).toBe("TAKO_RPC_ERROR");
      expect(err.message).toBe("unknown workflow 'x'");
    } finally {
      server.close();
    }
  });
});
