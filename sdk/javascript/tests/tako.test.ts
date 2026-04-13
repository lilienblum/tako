import { describe, expect, test, beforeEach, afterEach } from "bun:test";
import { Tako } from "../src/tako";

describe("Tako", () => {
  beforeEach(() => {
    // Reset singleton
    (Tako as any).instance = null;
    Tako.channels.clear();
  });

  afterEach(() => {
    delete process.env.TAKO_BUILD;
    Tako.channels.clear();
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

  describe("build", () => {
    test("returns unknown when TAKO_BUILD not set", () => {
      expect(Tako.build).toBe("unknown");
    });

    test("returns TAKO_BUILD value", () => {
      process.env.TAKO_BUILD = "v42";
      expect(Tako.build).toBe("v42");
    });
  });

  describe("isRunningInTako", () => {
    test("returns false when not in Tako environment", () => {
      expect(Tako.isRunningInTako()).toBe(false);
    });

    test("returns true when TAKO_BUILD is set", () => {
      process.env.TAKO_BUILD = "v1";
      expect(Tako.isRunningInTako()).toBe(true);
    });
  });
});
