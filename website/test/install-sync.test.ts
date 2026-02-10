import { describe, expect, it } from "bun:test";
import { access } from "node:fs/promises";
import { constants } from "node:fs";
import { join } from "node:path";

describe("installer script hosting", () => {
  it("does not keep legacy installer copies in website/public", async () => {
    const cliPublicPath = join(import.meta.dir, "..", "public", "install");
    const serverPublicPath = join(import.meta.dir, "..", "public", "install-server");

    await expect(access(cliPublicPath, constants.F_OK)).rejects.toThrow();
    await expect(access(serverPublicPath, constants.F_OK)).rejects.toThrow();
  });
});
