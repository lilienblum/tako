import { resolveUserAppImportUrl } from "../src/wrapper";
import { connectToServer } from "../src/wrapper";

test("resolveUserAppImportUrl resolves relative paths against cwd", () => {
  const cwd = process.cwd();
  const url = resolveUserAppImportUrl("./index.ts");
  expect(url.startsWith("file:")).toBe(true);
  expect(decodeURIComponent(url)).toContain(cwd);
});

test("connectToServer is silent when TAKO_APP_SOCKET is unset", async () => {
  const logSpy = jest.spyOn(console, "log").mockImplementation(() => {});
  await connectToServer({});

  expect(logSpy).not.toHaveBeenCalledWith("No TAKO_APP_SOCKET set - skipping server connection");

  logSpy.mockRestore();
});
