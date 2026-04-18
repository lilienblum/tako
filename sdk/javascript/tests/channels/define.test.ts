import { describe, expect, test } from "bun:test";
import { defineChannel, isChannelDefinition, CHANNEL_SYMBOL } from "../../src/channels/define";

describe("defineChannel", () => {
  test("returns a tagged definition", () => {
    const def = defineChannel("status", {
      auth: async () => true,
    });
    expect(def.type).toBe(CHANNEL_SYMBOL);
    expect(def.pattern).toBe("status");
    expect(typeof def.auth).toBe("function");
    expect(def.handler).toBeUndefined();
  });

  test("preserves handler map when provided", () => {
    const def = defineChannel<{ msg: { text: string } }>("chat/:roomId", {
      auth: async () => ({ subject: "u1" }),
      handler: {
        msg: async (data) => data,
      },
    });
    expect(def.handler).toBeDefined();
    expect(typeof def.handler!.msg).toBe("function");
  });

  test("passes through lifecycle config", () => {
    const def = defineChannel("status", {
      auth: async () => true,
      replayWindowMs: 1000,
      inactivityTtlMs: 2000,
      keepaliveIntervalMs: 3000,
      maxConnectionLifetimeMs: 4000,
    });
    expect(def.replayWindowMs).toBe(1000);
    expect(def.inactivityTtlMs).toBe(2000);
    expect(def.keepaliveIntervalMs).toBe(3000);
    expect(def.maxConnectionLifetimeMs).toBe(4000);
  });

  test("rejects invalid pattern at define time", () => {
    expect(() => defineChannel("/bad", { auth: async () => true })).toThrow(/must not start/);
  });

  test("auth defaults to allow-all when omitted", async () => {
    const def = defineChannel("public");
    const verdict = await def.auth(new Request("http://localhost/channels/public"), {
      channel: "public",
      operation: "subscribe",
      pattern: "public",
      params: {},
    });
    expect(verdict).toBe(true);
  });

  test("auth is also optional with other config fields", async () => {
    const def = defineChannel<{ ping: { at: number } }>("status", {
      handler: { ping: async (d) => d },
      replayWindowMs: 1000,
    });
    expect(def.handler).toBeDefined();
    expect(def.replayWindowMs).toBe(1000);
    const verdict = await def.auth(new Request("http://localhost/channels/status"), {
      channel: "status",
      operation: "subscribe",
      pattern: "status",
      params: {},
    });
    expect(verdict).toBe(true);
  });
});

describe("isChannelDefinition", () => {
  test("true for output of defineChannel", () => {
    const def = defineChannel("status", { auth: async () => true });
    expect(isChannelDefinition(def)).toBe(true);
  });

  test("false for plain objects", () => {
    expect(isChannelDefinition({ pattern: "status", auth: () => true })).toBe(false);
    expect(isChannelDefinition(null)).toBe(false);
    expect(isChannelDefinition(undefined)).toBe(false);
    expect(isChannelDefinition("string")).toBe(false);
  });
});
