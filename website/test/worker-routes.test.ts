import { describe, expect, it } from "bun:test";
import { access } from "node:fs/promises";
import { constants } from "node:fs";
import { join } from "node:path";

describe("worker config", () => {
  it("does not include a custom worker entrypoint", async () => {
    const workerPath = join(import.meta.dir, "..", "src", "worker.ts");
    await expect(access(workerPath, constants.F_OK)).rejects.toThrow();
  });
});
