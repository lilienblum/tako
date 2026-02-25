import { resolveAppSocketPath } from "../src/socket-path";

test("resolveAppSocketPath returns undefined when unset", () => {
  expect(resolveAppSocketPath(undefined)).toBeUndefined();
});

test("resolveAppSocketPath leaves non-template values unchanged", () => {
  expect(resolveAppSocketPath("/tmp/tako.sock", 1234)).toBe("/tmp/tako.sock");
});

test("resolveAppSocketPath replaces pid placeholder", () => {
  expect(resolveAppSocketPath("/tmp/tako-app-demo-{pid}.sock", 4242)).toBe(
    "/tmp/tako-app-demo-4242.sock",
  );
});
