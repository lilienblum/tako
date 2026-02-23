import { resolveUserAppImportUrl } from "../src/wrapper";

test("resolveUserAppImportUrl resolves relative paths against cwd", () => {
  const cwd = process.cwd();
  const url = resolveUserAppImportUrl("./index.ts");
  expect(url.startsWith("file:")).toBe(true);
  expect(decodeURIComponent(url)).toContain(cwd);
});
