import { describe, expect, test } from "bun:test";
import {
  defineChannel,
  isChannelDefinition,
  isChannelExport,
  CHANNEL_SYMBOL,
} from "../../src/channels/define";

describe("defineChannel", () => {
  test("returns an export whose definition is tagged", () => {
    const exp = defineChannel("status", { auth: async () => true });
    expect(exp.definition.type).toBe(CHANNEL_SYMBOL);
    expect(exp.definition.pattern).toBe("status");
    expect(typeof exp.definition.auth).toBe("function");
    expect(exp.definition.handler).toBeUndefined();
  });

  test("preserves handler map when provided", () => {
    const exp = defineChannel<{ msg: { text: string } }>("chat/:roomId", {
      auth: async () => ({ subject: "u1" }),
      handler: {
        msg: async (data) => data,
      },
    });
    expect(exp.definition.handler).toBeDefined();
    expect(typeof exp.definition.handler!.msg).toBe("function");
  });

  test("passes through lifecycle config", () => {
    const exp = defineChannel("status", {
      auth: async () => true,
      replayWindowMs: 1000,
      inactivityTtlMs: 2000,
      keepaliveIntervalMs: 3000,
      maxConnectionLifetimeMs: 4000,
    });
    expect(exp.definition.replayWindowMs).toBe(1000);
    expect(exp.definition.inactivityTtlMs).toBe(2000);
    expect(exp.definition.keepaliveIntervalMs).toBe(3000);
    expect(exp.definition.maxConnectionLifetimeMs).toBe(4000);
  });

  test("rejects invalid pattern at define time", () => {
    expect(() => defineChannel("/bad", { auth: async () => true })).toThrow(/must not start/);
  });

  test("auth defaults to allow-all when omitted", async () => {
    const exp = defineChannel("public");
    const verdict = await exp.definition.auth(new Request("http://localhost/channels/public"), {
      channel: "public",
      operation: "subscribe",
      pattern: "public",
      params: {},
    });
    expect(verdict).toBe(true);
  });

  test("auth is also optional with other config fields", async () => {
    const exp = defineChannel<{ ping: { at: number } }>("status", {
      handler: { ping: async (d) => d },
      replayWindowMs: 1000,
    });
    expect(exp.definition.handler).toBeDefined();
    expect(exp.definition.replayWindowMs).toBe(1000);
    const verdict = await exp.definition.auth(new Request("http://localhost/channels/status"), {
      channel: "status",
      operation: "subscribe",
      pattern: "status",
      params: {},
    });
    expect(verdict).toBe(true);
  });

  test("exposes a type-only $messageTypes narrower", () => {
    const exp = defineChannel("status").$messageTypes<{ ping: { at: number } }>();
    expect(exp.definition.pattern).toBe("status");
  });
});

describe("isChannelExport", () => {
  test("true for output of defineChannel", () => {
    const exp = defineChannel("status", { auth: async () => true });
    expect(isChannelExport(exp)).toBe(true);
  });

  test("false for plain objects and bare definitions", () => {
    expect(isChannelExport({ pattern: "status", auth: () => true })).toBe(false);
    expect(isChannelExport(null)).toBe(false);
    expect(isChannelExport(undefined)).toBe(false);
    expect(isChannelExport("string")).toBe(false);
  });
});

describe("isChannelDefinition", () => {
  test("true for the inner definition of a defineChannel result", () => {
    const exp = defineChannel("status", { auth: async () => true });
    expect(isChannelDefinition(exp.definition)).toBe(true);
  });

  test("false for plain objects", () => {
    expect(isChannelDefinition({ pattern: "status", auth: () => true })).toBe(false);
    expect(isChannelDefinition(null)).toBe(false);
    expect(isChannelDefinition(undefined)).toBe(false);
    expect(isChannelDefinition("string")).toBe(false);
  });
});
