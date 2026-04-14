import { afterEach, expect, test } from "bun:test";
import { Tako } from "../src/tako";

afterEach(() => {
  Tako.channels.clear();
});

test("Tako exposes channels and secrets at module load time", () => {
  expect(Tako.channels).toBeDefined();
  expect(typeof Tako.channels.define).toBe("function");
  expect(Tako.secrets).toBeDefined();
});
