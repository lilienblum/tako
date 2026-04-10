/**
 * Tests for `tako doctor` output formatting.
 *
 * Doctor produces rich structured output with sections, labels,
 * status values, and hints — exercising heading(), row(), hint(),
 * brand_success(), brand_warning(), brand_muted().
 */

import { describe, test, expect, beforeEach, afterEach } from "bun:test";
import { TakoTerminal, run } from "../helpers/terminal";
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

describe("tako doctor", () => {
  test("shows Paths section with Config and Data labels", async () => {
    const { screen, exitCode } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(exitCode).toBe(0);
    expect(screen).toContain("Paths");
    expect(screen).toContain("Config");
    expect(screen).toContain("Data");
    // Path may wrap across lines in 80-col terminal, so check for .tako suffix
    expect(screen).toContain(".tako");
  });

  test("shows Local CA section with status", async () => {
    const { screen } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(screen).toContain("Local CA");
    expect(screen).toContain("Status");
    expect(screen).toContain("not created");
  });

  test("shows Development server section", async () => {
    const { screen } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(screen).toContain("Development server");
    expect(screen).toContain("not running");
  });

  test("section headings are bold", async () => {
    const { term } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    const pathsRow = findRowContaining(term, "Paths");
    expect(pathsRow).not.toBeNull();

    if (pathsRow !== null) {
      const col = findCharInRow(term, pathsRow, "P");
      if (col !== null) {
        const cell = term.cell(pathsRow, col);
        expect(cell).not.toBeNull();
        expect(cell!.isBold).toBe(true);
      }
    }
  });

  test("hint lines are dimmed", async () => {
    const { term } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    // Hints under Config/Data like "Directory where Tako stores..."
    const hintRow = findRowContaining(term, "Directory where Tako stores");

    if (hintRow !== null) {
      const col = findNonSpaceCol(term, hintRow);
      if (col !== null) {
        const cell = term.cell(hintRow, col);
        expect(cell).not.toBeNull();
        expect(cell!.isDim).toBe(true);
      }
    }
  });

  test("status 'not created' has warning color", async () => {
    const { term } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    const row = findRowContaining(term, "not created");
    expect(row).not.toBeNull();

    if (row !== null) {
      const col = findSubstringCol(term, row, "not");
      if (col !== null) {
        const rgb = term.fgRgb(row, col);
        expect(rgb).not.toBeNull();
        // BRAND_AMBER = (234, 211, 156)
        expect(rgb![0]).toBeGreaterThan(200); // R
        expect(rgb![1]).toBeGreaterThan(180); // G
        expect(rgb![2]).toBeGreaterThan(120); // B
      }
    }
  });

  test("status 'not running' has warning color", async () => {
    const { term } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    const row = findRowContaining(term, "not running");
    expect(row).not.toBeNull();

    if (row !== null) {
      const col = findSubstringCol(term, row, "not");
      if (col !== null) {
        const rgb = term.fgRgb(row, col);
        expect(rgb).not.toBeNull();
        // BRAND_AMBER
        expect(rgb![0]).toBeGreaterThan(200);
      }
    }
  });
});

// Platform-specific sections
const isLinux = process.platform === "linux";
const isMacOS = process.platform === "darwin";

describe.if(isLinux)("tako doctor (Linux)", () => {
  test("shows Port Redirect section", async () => {
    const { screen } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(screen).toContain("Port Redirect");
    expect(screen).toContain("Alias");
    expect(screen).toContain("Persistence");
  });

  test("does not show Dev Proxy section", async () => {
    const { screen } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(screen).not.toContain("Dev Proxy");
    expect(screen).not.toContain("Launchd");
  });
});

describe.if(isMacOS)("tako doctor (macOS)", () => {
  test("shows Dev Proxy section", async () => {
    const { screen } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(screen).toContain("Dev Proxy");
  });

  test("does not show Port Redirect section", async () => {
    const { screen } = await run(["doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(screen).not.toContain("Port Redirect");
  });
});

describe("tako doctor --ci", () => {
  test("produces no ANSI RGB color codes", async () => {
    const { term, exitCode } = await run(["--ci", "doctor"], {
      cwd: tempDir,
      env: { HOME: tempDir, TAKO_HOME: takoHome },
    });

    expect(exitCode).toBe(0);
    const raw = term.rawOutput();
    // eslint-disable-next-line no-control-regex
    expect(raw).not.toMatch(/\x1b\[38;2;\d+;\d+;\d+m/);
  });
});

// ── Helpers ─────────────────────────────────────────────────────────

function findRowContaining(term: TakoTerminal, text: string): number | null {
  const totalRows = term.terminal.buffer.active.length;
  for (let y = 0; y < totalRows; y++) {
    if (term.row(y).includes(text)) return y;
  }
  return null;
}

function findCharInRow(term: TakoTerminal, row: number, char: string): number | null {
  for (let x = 0; x < 80; x++) {
    const c = term.cell(row, x);
    if (c && c.char === char) return x;
  }
  return null;
}

function findNonSpaceCol(term: TakoTerminal, row: number): number | null {
  for (let x = 0; x < 80; x++) {
    const c = term.cell(row, x);
    if (c && c.char.trim() !== "") return x;
  }
  return null;
}

function findSubstringCol(term: TakoTerminal, row: number, text: string): number | null {
  const rowText = term.row(row, false);
  const idx = rowText.indexOf(text);
  return idx >= 0 ? idx : null;
}
