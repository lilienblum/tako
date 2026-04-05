import { afterEach, expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { tmpdir } from "node:os";

import { createEntrypoint } from "../src/create-entrypoint";

const originalArgv = [...process.argv];

afterEach(() => {
  process.argv = [...originalArgv];
});

test("createEntrypoint returns run function and config", () => {
  const { run, port, setDraining } = createEntrypoint();
  expect(typeof run).toBe("function");
  expect(typeof port).toBe("number");
  expect(typeof setDraining).toBe("function");
});

test("createEntrypoint awaits optional ready hook before starting server", async () => {
  const rootDir = await mkdtemp(path.join(tmpdir(), "tako-entrypoint-"));
  const entryModule = path.join(rootDir, "entry.mjs");
  const lifecycle: string[] = [];

  try {
    (
      globalThis as typeof globalThis & { __takoEntrypointLifecycle?: string[] }
    ).__takoEntrypointLifecycle = lifecycle;
    await writeFile(
      entryModule,
      [
        "const lifecycle = globalThis.__takoEntrypointLifecycle;",
        "export default {",
        "  async ready() {",
        '    lifecycle?.push("ready");',
        "  },",
        "  async fetch() {",
        '    lifecycle?.push("fetch");',
        '    return new Response("ok");',
        "  },",
        "};",
        "",
      ].join("\n"),
      "utf8",
    );

    process.argv = ["node", "entrypoint", entryModule, "--instance", "i-1", "--version", "v-1"];

    const { run } = createEntrypoint();
    await run(async () => {
      lifecycle.push("start-server");
      return 4321;
    });

    expect(lifecycle).toEqual(["ready", "start-server"]);
  } finally {
    delete (globalThis as typeof globalThis & { __takoEntrypointLifecycle?: string[] })
      .__takoEntrypointLifecycle;
    await rm(rootDir, { recursive: true, force: true });
  }
});
