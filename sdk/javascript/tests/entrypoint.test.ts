import { test, expect } from "bun:test";
import { createEntrypoint } from "../src/create-entrypoint";

test("createEntrypoint returns run function and config", () => {
  const { run, port, setDraining } = createEntrypoint();
  expect(typeof run).toBe("function");
  expect(typeof port).toBe("number");
  expect(typeof setDraining).toBe("function");
});
