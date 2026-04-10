/**
 * E2E tests for `tako dev` — runs against real fixtures (bun, node, deno).
 * Skipped unless TAKO_DEV_E2E=1 is set.
 */
import { describe, test, expect } from "bun:test";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, cpSync, symlinkSync } from "fs";
import { join, resolve } from "path";
import { tmpdir } from "os";

const SKIP = !process.env.TAKO_DEV_E2E;
const TAKO_BIN =
  process.env.TAKO_BIN ?? resolve(import.meta.dirname, "..", "..", "..", "target", "debug", "tako");
const FIXTURES_DIR = resolve(import.meta.dirname, "..", "..", "fixtures", "javascript");
const SDK_DIR = resolve(import.meta.dirname, "..", "..", "..", "sdk", "javascript");

function safeRead(path: string): string {
  try {
    return readFileSync(path, "utf-8");
  } catch {
    return "";
  }
}

/** Copy a fixture to a temp dir and symlink the SDK. */
function prepareFixture(name: string) {
  const tempDir = mkdtempSync(join(tmpdir(), `tako-dev-e2e-${name}-`));
  const pd = join(tempDir, "app");
  cpSync(join(FIXTURES_DIR, name), pd, { recursive: true });

  // Symlink the SDK so the entrypoint is available without npm install.
  mkdirSync(join(pd, "node_modules"), { recursive: true });
  const sdkLink = join(pd, "node_modules", "tako.sh");
  rmSync(sdkLink, { recursive: true, force: true });
  symlinkSync(SDK_DIR, sdkLink);

  const lf = join(tempDir, "dev.log");
  return { tempDir, pd, lf };
}

function startDev(pd: string, lf: string) {
  return Bun.spawn(["sh", "-c", `exec "${TAKO_BIN}" dev > "${lf}" 2>&1`], {
    cwd: pd,
    env: { ...process.env, TERM: "dumb", NO_COLOR: "1" },
    stdin: "ignore",
    stdout: "ignore",
    stderr: "ignore",
  });
}

async function waitForApp(lf: string, timeoutMs = 60_000): Promise<number> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const log = safeRead(lf);
    const m = log.match(/Application listening on http:\/\/[\d.]+:(\d+)/);
    if (m) return Number(m[1]);
    await Bun.sleep(300);
  }
  throw new Error(`App didn't start.\nLog:\n${safeRead(lf)}`);
}

describe.skipIf(SKIP)("tako dev fixtures", () => {
  // Deno resolves node_modules differently (downloads from npm, ignores symlinks).
  // Skip until we have a proper SDK install for deno fixtures.
  for (const runtime of ["bun", "node"]) {
    test(`${runtime}: starts and serves HTTP`, async () => {
      const { tempDir, pd, lf } = prepareFixture(runtime);
      const proc = startDev(pd, lf);

      try {
        const port = await waitForApp(lf);

        // Fixtures serve HTML at /.
        const appName = `${runtime}-e2e`;
        const resp = await fetch(`http://127.0.0.1:${port}/`, {
          headers: { Host: `${appName}.test` },
        });
        expect(resp.status).toBe(200);
        const body = await resp.text();
        expect(body).toContain("Tako app");
      } finally {
        try {
          process.kill(proc.pid, "SIGKILL");
        } catch {}
        rmSync(tempDir, { recursive: true, force: true });
      }
    }, 90_000);
  }

  test("bun: detects process exit", async () => {
    const { tempDir, pd, lf } = prepareFixture("bun");
    const proc = startDev(pd, lf);

    try {
      const port = await waitForApp(lf);

      // Find the app's PID via lsof and kill it.
      const lsof = Bun.spawn(["lsof", "-ti", `tcp:${port}`], {
        stdout: "pipe",
        stderr: "ignore",
      });
      const pids = (await new Response(lsof.stdout).text())
        .trim()
        .split("\n")
        .filter(Boolean)
        .map(Number)
        .filter((p) => p !== proc.pid);
      await lsof.exited;

      expect(pids.length).toBeGreaterThan(0);
      for (const pid of pids) {
        try {
          process.kill(pid, "SIGKILL");
        } catch {}
      }

      // Wait for exit detection.
      for (let i = 0; i < 20; i++) {
        await Bun.sleep(500);
        if (/app exited \(killed by signal/.test(safeRead(lf))) break;
      }
      expect(safeRead(lf)).toMatch(/app exited \(killed by signal/);
    } finally {
      try {
        process.kill(proc.pid, "SIGKILL");
      } catch {}
      rmSync(tempDir, { recursive: true, force: true });
    }
  }, 90_000);
});
