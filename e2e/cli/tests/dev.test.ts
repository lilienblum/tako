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

/**
 * Wait for the dev server to be ready.
 * Returns the dev URL printed by `tako dev` (e.g. https://bun-e2e.test/).
 * Readiness is signalled by "App started" in the log.
 */
async function waitForApp(lf: string, timeoutMs = 60_000): Promise<string> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const log = safeRead(lf);
    if (/App started/.test(log)) {
      const m = log.match(/^(https?:\/\/\S+)/m);
      if (m) return m[1];
    }
    await Bun.sleep(300);
  }
  throw new Error(`App didn't start.\nLog:\n${safeRead(lf)}`);
}

/**
 * Wait for the app process PID to appear in the log ("App pid <n>").
 * The runner emits this line in non-interactive mode.
 */
async function waitForAppPid(lf: string, timeoutMs = 30_000): Promise<number> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const m = safeRead(lf).match(/^App pid (\d+)/m);
    if (m) return Number(m[1]);
    await Bun.sleep(300);
  }
  throw new Error(`App pid never appeared.\nLog:\n${safeRead(lf)}`);
}

describe.skipIf(SKIP)("tako dev fixtures", () => {
  // Deno resolves node_modules differently (downloads from npm, ignores symlinks).
  // Skip until we have a proper SDK install for deno fixtures.
  for (const runtime of ["bun", "node"]) {
    test(`${runtime}: starts and serves HTTP`, async () => {
      const { tempDir, pd, lf } = prepareFixture(runtime);
      const proc = startDev(pd, lf);

      try {
        const devUrl = await waitForApp(lf);

        // Fixtures serve HTML at /.
        const resp = await fetch(devUrl, {
          // @ts-ignore — Bun extension: skip TLS verification for the self-signed dev CA
          tls: { rejectUnauthorized: false },
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
      await waitForApp(lf);

      // Wait for the app PID to appear in the log, then kill it directly.
      const appPid = await waitForAppPid(lf);
      try {
        process.kill(appPid, "SIGKILL");
      } catch {}

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
