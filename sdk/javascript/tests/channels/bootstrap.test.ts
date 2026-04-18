import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { bootstrapChannels } from "../../src/channels/bootstrap";
import { ChannelRegistry } from "../../src/channels";

let appDir = "";

beforeEach(async () => {
  appDir = await mkdtemp(join(tmpdir(), "tako-ch-boot-"));
});

afterEach(async () => {
  await rm(appDir, { recursive: true, force: true });
});

function sdkImportPath(): string {
  return join(import.meta.dir, "..", "..", "src", "channels", "define.ts");
}

describe("bootstrapChannels", () => {
  test("no-op when channels/ does not exist", async () => {
    const reg = new ChannelRegistry();
    const result = await bootstrapChannels({ appDir, registry: reg });
    expect(result.channelCount).toBe(0);
    expect(reg.resolve("x")).toBeNull();
  });

  test("registers discovered channels", async () => {
    await mkdir(join(appDir, "channels"));
    await writeFile(
      join(appDir, "channels", "status.ts"),
      `import { defineChannel } from "${sdkImportPath()}";
       export default defineChannel("status", { auth: async () => true });`,
      "utf8",
    );
    const reg = new ChannelRegistry();
    const result = await bootstrapChannels({ appDir, registry: reg });
    expect(result.channelCount).toBe(1);
    expect(reg.resolve("status")).not.toBeNull();
  });

  test("clears the registry before registering", async () => {
    await mkdir(join(appDir, "channels"));
    await writeFile(
      join(appDir, "channels", "status.ts"),
      `import { defineChannel } from "${sdkImportPath()}";
       export default defineChannel("status", { auth: async () => true });`,
      "utf8",
    );
    const reg = new ChannelRegistry();
    await bootstrapChannels({ appDir, registry: reg });
    await bootstrapChannels({ appDir, registry: reg });
    expect(reg.all.length).toBe(1);
  });
});
