import { describe, test, expect, beforeEach, afterEach } from "bun:test";
import { TakoTerminal, run } from "../helpers/terminal";
import { mkdtemp, writeFile, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

let tempDir: string;

beforeEach(async () => {
  tempDir = await mkdtemp(join(tmpdir(), "tako-cli-test-"));
});

afterEach(async () => {
  await rm(tempDir, { recursive: true, force: true });
});

describe("tako init --ci", () => {
  test("creates tako.toml in non-interactive mode", async () => {
    await writeFile(
      join(tempDir, "package.json"),
      JSON.stringify({ name: "test-app" }),
    );

    const takoHome = join(tempDir, ".tako");
    const { exitCode } = await run(["--ci", "init"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(exitCode).toBe(0);
    const toml = await readFile(join(tempDir, "tako.toml"), "utf-8");
    expect(toml).toContain('name = "test-app"');
  });

  test("detects bun runtime from bun.lock", async () => {
    await writeFile(
      join(tempDir, "package.json"),
      JSON.stringify({ name: "bun-app" }),
    );
    await writeFile(join(tempDir, "bun.lock"), "");

    const takoHome = join(tempDir, ".tako");
    const { exitCode } = await run(["--ci", "init"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(exitCode).toBe(0);
    const toml = await readFile(join(tempDir, "tako.toml"), "utf-8");
    expect(toml).toContain('runtime = "bun"');
  });

  test("--ci produces no ANSI color codes in output", async () => {
    await writeFile(
      join(tempDir, "package.json"),
      JSON.stringify({ name: "test-app" }),
    );

    const takoHome = join(tempDir, ".tako");
    const { term, exitCode } = await run(["--ci", "init"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(exitCode).toBe(0);
    const raw = term.rawOutput();
    // No RGB color sequences
    expect(raw).not.toMatch(/\x1b\[38;2;\d+;\d+;\d+m/);
  });
});

describe("tako init (interactive wizard)", () => {
  test("shows wizard prompts in PTY", async () => {
    await writeFile(
      join(tempDir, "package.json"),
      JSON.stringify({ name: "wizard-app" }),
    );

    const takoHome = join(tempDir, ".tako");
    const term = TakoTerminal.spawn({
      args: ["init"],
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    // The wizard starts — should show the first prompt
    await term.waitForText("Application name", { timeout: 5000 });

    // Screen should have colored output (we're in a real PTY)
    const screen = term.screenText();
    expect(screen).toContain("Application name");

    // Exit the wizard
    term.press("\x03");
    await term.close();
  });
});
