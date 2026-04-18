import { afterEach, expect, test } from "bun:test";
import { Logger } from "../src/logger";
import { Tako } from "../src/tako";

afterEach(() => {
  Tako.channels.clear();
});

test("Tako exposes channels and secrets at module load time", () => {
  expect(Tako.channels).toBeDefined();
  expect(typeof Tako.channels.register).toBe("function");
  expect(typeof Tako.channels.authorize).toBe("function");
  expect(Tako.secrets).toBeDefined();
});

test("Tako.logger is a Logger instance with source 'app'", () => {
  expect(Tako.logger).toBeInstanceOf(Logger);
  expect(typeof Tako.logger.info).toBe("function");
  expect(typeof Tako.logger.child).toBe("function");
  expect(typeof Tako.logger.setGlobals).toBe("function");
});
