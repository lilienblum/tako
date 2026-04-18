import { describe, expect, test } from "bun:test";
import { compilePattern, matchPattern, comparePatterns } from "../../src/channels/pattern";

describe("compilePattern", () => {
  test("rejects empty pattern", () => {
    expect(() => compilePattern("")).toThrow(/pattern must not be empty/);
  });

  test("rejects leading or trailing slash", () => {
    expect(() => compilePattern("/chat")).toThrow(/must not start/);
    expect(() => compilePattern("chat/")).toThrow(/must not end/);
  });

  test("rejects empty segments", () => {
    expect(() => compilePattern("chat//rooms")).toThrow(/empty segment/);
  });

  test("rejects bare param name", () => {
    expect(() => compilePattern(":roomId")).toThrow(/must begin with/);
  });

  test("collects param names in order", () => {
    const p = compilePattern("chat/:roomId/users/:userId");
    expect(p.paramNames).toEqual(["roomId", "userId"]);
  });

  test("rejects duplicate param names", () => {
    expect(() => compilePattern("chat/:id/reply/:id")).toThrow(/duplicate param/);
  });

  test("accepts trailing wildcard", () => {
    const p = compilePattern("chat/:roomId/*");
    expect(p.hasWildcard).toBe(true);
  });

  test("rejects wildcard mid-pattern", () => {
    expect(() => compilePattern("chat/*/typing")).toThrow(/wildcard must be the final segment/);
  });
});

describe("matchPattern", () => {
  test("literal exact match", () => {
    const p = compilePattern("status");
    expect(matchPattern(p, "status")).toEqual({ params: {} });
    expect(matchPattern(p, "status/x")).toBeNull();
  });

  test("param capture", () => {
    const p = compilePattern("chat/:roomId");
    expect(matchPattern(p, "chat/abc")).toEqual({ params: { roomId: "abc" } });
    expect(matchPattern(p, "chat/")).toBeNull();
    expect(matchPattern(p, "chat/abc/extra")).toBeNull();
  });

  test("multi-param capture", () => {
    const p = compilePattern("chat/:roomId/users/:userId");
    expect(matchPattern(p, "chat/r1/users/u1")).toEqual({ params: { roomId: "r1", userId: "u1" } });
    expect(matchPattern(p, "chat/r1/users")).toBeNull();
  });

  test("trailing wildcard", () => {
    const p = compilePattern("chat/:roomId/*");
    expect(matchPattern(p, "chat/r1/typing")).toEqual({ params: { roomId: "r1" } });
    expect(matchPattern(p, "chat/r1/a/b/c")).toEqual({ params: { roomId: "r1" } });
    expect(matchPattern(p, "chat/r1")).toBeNull();
  });

  test("params cannot contain slashes", () => {
    const p = compilePattern("chat/:roomId");
    expect(matchPattern(p, "chat/a/b")).toBeNull();
  });
});

describe("comparePatterns specificity", () => {
  test("literal beats param at same position", () => {
    const a = compilePattern("chat/lobby");
    const b = compilePattern("chat/:roomId");
    expect(comparePatterns(a, b)).toBeLessThan(0);
    expect(comparePatterns(b, a)).toBeGreaterThan(0);
  });

  test("param beats wildcard", () => {
    const a = compilePattern("chat/:roomId");
    const b = compilePattern("chat/*");
    expect(comparePatterns(a, b)).toBeLessThan(0);
  });

  test("longer literal prefix wins", () => {
    const a = compilePattern("chat/rooms/:id");
    const b = compilePattern("chat/:scope/:id");
    expect(comparePatterns(a, b)).toBeLessThan(0);
  });

  test("ties are zero", () => {
    const a = compilePattern("chat/:roomId");
    const b = compilePattern("chat/:otherName");
    expect(comparePatterns(a, b)).toBe(0);
  });
});
