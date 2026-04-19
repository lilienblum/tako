import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  APP_NAME_ENV,
  assertInternalSocketEnvConsistency,
  INTERNAL_SOCKET_ENV,
  internalSocketFromEnv,
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
