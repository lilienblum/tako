import { describe, test, expect, beforeEach, afterEach } from "bun:test";
import { TakoTerminal } from "../helpers/terminal";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

let tempDir: string;
let takoHome: string;

beforeEach(async () => {
  tempDir = await mkdtemp(join(tmpdir(), "tako-cli-test-"));
  takoHome = join(tempDir, ".tako");
});

afterEach(async () => {
  await rm(tempDir, { recursive: true, force: true });
});

function spawnServerAdd() {
  return TakoTerminal.spawn({
    args: ["servers", "add", "--no-test"],
    cwd: tempDir,
    env: { HOME: tempDir, TAKO_HOME: takoHome },
  });
}

describe("server add wizard", () => {
  test("Ctrl+C collapses an optional prompt without leaving its hint behind", async () => {
    const term = spawnServerAdd();

    await term.waitForText("Server IP or hostname", { timeout: 5000 });
    term.write("127.0.0.1\r");

    await term.waitForText("SSH port", { timeout: 5000 });
    term.press("\r");

    await term.waitForText("Server name", { timeout: 5000 });
    term.write("prod\r");

    await term.waitForText("Description", { timeout: 5000 });
    await term.waitForText("optional", { timeout: 5000 });

    term.press("\x03");
    await term.waitForText("Operation cancelled", { timeout: 5000 });

    const labelRow = findRowContaining(term, "Description");
    const cancelledRow = findRowContaining(term, "Operation cancelled");
    const screen = term.screenText();

    expect(labelRow).not.toBeNull();
    expect(cancelledRow).toBe(labelRow! + 2);
    expect(term.row(labelRow! + 1)).toBe("");
    expect(term.row(labelRow!)).not.toContain("›");
    expect(screen).not.toContain("optional");
    expect(term.rawOutput()).toContain("\x1b[9m");

    const exitCode = await term.waitForExit({ timeout: 5000 });
    expect([0, 130]).toContain(exitCode);
  });
});

function findRowContaining(term: TakoTerminal, text: string): number | null {
  const fullText = term.fullText();
  const lines = fullText.split("\n");
  for (let y = 0; y < lines.length; y++) {
    if (lines[y].includes(text)) return y;
  }
  return null;
}
