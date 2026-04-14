import { afterEach, expect, test } from "bun:test";
import { mkdtemp, open, rm, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { spawn } from "node:child_process";
import path from "node:path";
import { tmpdir } from "node:os";

import { createEntrypoint, signalReadyPortOnFd } from "../src/create-entrypoint";

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

test("readiness signal writes the bound port to a pipe fd instead of stdout", async () => {
  const rootDir = await mkdtemp(path.join(tmpdir(), "tako-ready-fd-"));
  const readyPipe = path.join(rootDir, "ready.pipe");

  try {
    const mkfifo = spawn("mkfifo", [readyPipe], {
      cwd: path.resolve(import.meta.dir, ".."),
      stdio: ["ignore", "ignore", "pipe"],
    });
    await new Promise<void>((resolve, reject) => {
      mkfifo.once("exit", (code) => {
        if (code === 0) {
          resolve();
        } else {
          reject(new Error(`mkfifo exited with code ${code}`));
        }
      });
    });

    const readyReader = spawn("cat", [readyPipe], {
      cwd: path.resolve(import.meta.dir, ".."),
      stdio: ["ignore", "pipe", "pipe"],
    });
    const readyLinePromise = Promise.race([
      new Promise<string>((resolve, reject) => {
        const stream = readyReader.stdout;
        if (!stream || typeof stream === "number") {
          reject(new Error("expected fd 4 pipe"));
          return;
        }

        const rl = createInterface({ input: stream });
        let sawLine = false;
        rl.once("line", (line) => {
          sawLine = true;
          rl.close();
          resolve(line);
        });
        rl.once("close", () => {
          if (!sawLine) {
            reject(new Error("fd 4 closed before readiness signal"));
          }
        });
      }),
      new Promise<string>((_, reject) =>
        setTimeout(() => reject(new Error("timed out waiting for readiness signal")), 5_000),
      ),
    ]);

    const readyWriter = await open(readyPipe, "w");
    signalReadyPortOnFd(readyWriter.fd, 4321);

    const readyLine = await readyLinePromise;
    expect(readyLine).toBe("4321");
    await new Promise<void>((resolve) => readyReader.once("exit", () => resolve()));
  } finally {
    await rm(rootDir, { recursive: true, force: true });
  }
});
