import { describe, expect, test, beforeEach, afterEach } from "bun:test";
import { Tako } from "../src/tako";

describe("Tako", () => {
  beforeEach(() => {
    // Reset singleton
    (Tako as any).instance = null;
  });

  afterEach(() => {
    // Clean up environment
    delete process.env.TAKO_VERSION;
    delete process.env.TAKO_INSTANCE;
    delete process.env.TAKO_BUILD;
    delete process.env.TAKO_APP_SOCKET;
  });

  test("creates instance with default options", () => {
    const tako = new Tako();
    expect(tako).toBeDefined();
    expect(tako.getOptions()).toEqual({});
  });

  test("stores as singleton", () => {
    const tako = new Tako();
    expect(Tako.getInstance()).toBe(tako);
  });

  test("replaces previous singleton", () => {
    const tako1 = new Tako();
    const tako2 = new Tako();
    expect(Tako.getInstance()).toBe(tako2);
    expect(Tako.getInstance()).not.toBe(tako1);
  });

  describe("getEnv", () => {
    test("returns default values when env not set", () => {
      const env = Tako.getEnv();
      expect(env.version).toBe("unknown");
      expect(env.instanceId).toBe(0);
    });

    test("returns values from environment", () => {
      process.env.TAKO_VERSION = "abc123";
      process.env.TAKO_INSTANCE = "2";

      const env = Tako.getEnv();
      expect(env.version).toBe("abc123");
      expect(env.instanceId).toBe(2);
    });
  });

  describe("isRunningInTako", () => {
    test("returns false when not in Tako environment", () => {
      expect(Tako.isRunningInTako()).toBe(false);
    });

    test("returns true when TAKO_VERSION is set", () => {
      process.env.TAKO_VERSION = "abc123";
      expect(Tako.isRunningInTako()).toBe(true);
    });
  });
});
