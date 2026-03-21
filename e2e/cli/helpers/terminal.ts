/**
 * PTY + headless terminal harness for CLI output testing.
 *
 * Spawns the tako binary inside a real pseudo-terminal (via Bun's native
 * PTY support) and pipes output into a headless xterm.js instance so
 * tests can assert on the *rendered* screen state — text, colors, cursor
 * position, bold/dim attributes — exactly as a user would see them.
 */

import { Terminal } from "@xterm/headless";
import { resolve } from "node:path";

// ── Defaults ────────────────────────────────────────────────────────

const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;
const DEFAULT_TIMEOUT_MS = 10_000;

const TAKO_BIN =
  process.env.TAKO_BIN ?? resolve(import.meta.dirname, "..", "..", "..", "target", "debug", "tako");

// ── Types ───────────────────────────────────────────────────────────

export interface SpawnOptions {
  args?: string[];
  cols?: number;
  rows?: number;
  env?: Record<string, string>;
  cwd?: string;
  /** Timeout for the entire session (ms). Default: 10 000 */
  timeout?: number;
}

export interface CellInfo {
  char: string;
  fg: number;
  bg: number;
  isBold: boolean;
  isDim: boolean;
  isItalic: boolean;
  isUnderline: boolean;
  isFgRGB: boolean;
  isBgRGB: boolean;
  isFgDefault: boolean;
  isBgDefault: boolean;
}

// ── TakoTerminal ────────────────────────────────────────────────────

export class TakoTerminal {
  readonly terminal: Terminal;
  private proc: ReturnType<typeof Bun.spawn>;
  private rawChunks: Buffer[] = [];
  private _exitCode: number | null = null;

  private constructor(terminal: Terminal, proc: ReturnType<typeof Bun.spawn>) {
    this.terminal = terminal;
    this.proc = proc;
  }

  /** Spawn `tako` with the given arguments inside a PTY. */
  static spawn(opts: SpawnOptions = {}): TakoTerminal {
    const cols = opts.cols ?? DEFAULT_COLS;
    const rows = opts.rows ?? DEFAULT_ROWS;

    const terminal = new Terminal({
      cols,
      rows,
      allowProposedApi: true,
      scrollback: 1000,
    });

    const env: Record<string, string> = {
      ...(process.env as Record<string, string>),
      TERM: "xterm-256color",
      FORCE_COLOR: "1",
      ...opts.env,
    };

    // We'll collect raw data via the terminal callback and a reference
    // to the TakoTerminal instance
    const rawChunks: Buffer[] = [];

    const proc = Bun.spawn([TAKO_BIN, ...(opts.args ?? [])], {
      cwd: opts.cwd ?? process.cwd(),
      env,
      terminal: {
        cols,
        rows,
        name: "xterm-256color",
        data(_term, data) {
          const buf = Buffer.from(data);
          rawChunks.push(buf);
          terminal.write(new Uint8Array(data));
        },
      },
    });

    const tt = new TakoTerminal(terminal, proc);
    tt.rawChunks = rawChunks;
    return tt;
  }

  // ── Waiting ─────────────────────────────────────────────────────

  /** Wait until `predicate(screenText)` returns true, or throw on timeout. */
  async waitFor(
    predicate: (screen: string) => boolean,
    opts: { timeout?: number; label?: string } = {},
  ): Promise<void> {
    const timeout = opts.timeout ?? DEFAULT_TIMEOUT_MS;
    const label = opts.label ?? "waitFor";
    const deadline = Date.now() + timeout;

    while (Date.now() < deadline) {
      if (predicate(this.screenText())) return;
      await sleep(50);
    }

    throw new Error(
      `${label}: timed out after ${timeout}ms.\n\nScreen contents:\n${this.screenText()}`,
    );
  }

  /** Wait until the given text appears anywhere on screen. */
  async waitForText(text: string, opts: { timeout?: number } = {}): Promise<void> {
    return this.waitFor((s) => s.includes(text), {
      ...opts,
      label: `waitForText(${JSON.stringify(text)})`,
    });
  }

  /** Wait for the process to exit. Returns exit code. */
  async waitForExit(opts: { timeout?: number } = {}): Promise<number> {
    const timeout = opts.timeout ?? DEFAULT_TIMEOUT_MS;
    const result = await Promise.race([
      this.proc.exited,
      sleep(timeout).then(() => {
        throw new Error(
          `waitForExit: timed out after ${timeout}ms.\n\nScreen contents:\n${this.screenText()}`,
        );
      }),
    ]);
    this._exitCode = result;
    return result;
  }

  /** Wait for no new output for `duration` ms (output has stabilized). */
  async waitForIdle(duration: number = 500): Promise<void> {
    let lastLen = this.rawBytes().length;
    let stableAt = Date.now();

    while (Date.now() - stableAt < duration) {
      await sleep(50);
      const curLen = this.rawBytes().length;
      if (curLen !== lastLen) {
        lastLen = curLen;
        stableAt = Date.now();
      }
    }
  }

  // ── Input ───────────────────────────────────────────────────────

  /** Send raw data to the PTY (as if the user typed it). */
  write(data: string): void {
    this.proc.terminal!.write(data);
  }

  /** Send a key press. Common keys: "r", "d", "\x03" (Ctrl+C). */
  press(key: string): void {
    this.write(key);
  }

  /** Resize the terminal. */
  resize(cols: number, rows: number): void {
    this.proc.terminal!.resize(cols, rows);
    this.terminal.resize(cols, rows);
  }

  // ── Screen inspection ─────────────────────────────────────────

  /** Full rendered screen text (all rows, trailing whitespace trimmed). */
  screenText(): string {
    const buf = this.terminal.buffer.active;
    const lines: string[] = [];
    for (let y = 0; y < this.terminal.rows; y++) {
      const line = buf.getLine(y);
      lines.push(line ? line.translateToString(true) : "");
    }
    // Trim trailing empty lines
    while (lines.length > 0 && lines[lines.length - 1] === "") {
      lines.pop();
    }
    return lines.join("\n");
  }

  /** Get text of a specific row (0-indexed). Trimmed by default. */
  row(y: number, trim = true): string {
    const line = this.terminal.buffer.active.getLine(y);
    if (!line) return "";
    return trim ? line.translateToString(true) : line.translateToString(false);
  }

  /** Get cell info at (row, col). */
  cell(row: number, col: number): CellInfo | null {
    const line = this.terminal.buffer.active.getLine(row);
    if (!line) return null;
    const cell = this.terminal.buffer.active.getNullCell();
    const result = line.getCell(col, cell);
    if (!result) return null;
    return {
      char: result.getChars(),
      fg: result.getFgColor(),
      bg: result.getBgColor(),
      isBold: result.isBold() !== 0,
      isDim: result.isDim() !== 0,
      isItalic: result.isItalic() !== 0,
      isUnderline: result.isUnderline() !== 0,
      isFgRGB: result.isFgRGB(),
      isBgRGB: result.isBgRGB(),
      isFgDefault: result.isFgDefault(),
      isBgDefault: result.isBgDefault(),
    };
  }

  /** Get the RGB foreground color of a cell as [r, g, b] or null if not RGB. */
  fgRgb(row: number, col: number): [number, number, number] | null {
    const c = this.cell(row, col);
    if (!c || !c.isFgRGB) return null;
    // xterm.js packs RGB into a single number: (r << 16) | (g << 8) | b
    const n = c.fg;
    return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
  }

  /** Cursor position as { x, y }. */
  cursor(): { x: number; y: number } {
    const buf = this.terminal.buffer.active;
    return { x: buf.cursorX, y: buf.cursorY };
  }

  /** Raw bytes received from the PTY (for debugging). */
  rawBytes(): Buffer {
    return Buffer.concat(this.rawChunks);
  }

  /** Raw output as string (includes all escape sequences). */
  rawOutput(): string {
    return this.rawBytes().toString("utf-8");
  }

  // ── Scrollback ────────────────────────────────────────────────

  /** Get all text including scrollback (not just the visible viewport). */
  fullText(): string {
    const buf = this.terminal.buffer.active;
    const lines: string[] = [];
    for (let y = 0; y < buf.length; y++) {
      const line = buf.getLine(y);
      lines.push(line ? line.translateToString(true) : "");
    }
    while (lines.length > 0 && lines[lines.length - 1] === "") {
      lines.pop();
    }
    return lines.join("\n");
  }

  // ── Lifecycle ─────────────────────────────────────────────────

  /** Kill the PTY process. */
  kill(signal?: number | NodeJS.Signals): void {
    this.proc.kill(signal);
  }

  /** Clean shutdown: kill if still running, wait for exit. */
  async close(): Promise<void> {
    try {
      this.proc.kill();
    } catch {
      // already dead
    }
    await this.proc.exited.catch(() => {});
    try {
      this.proc.terminal?.close();
    } catch {
      // already closed
    }
    this.terminal.dispose();
  }
}

// ── Helpers ─────────────────────────────────────────────────────────

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// ── Convenience one-shot runner ─────────────────────────────────────

/**
 * Spawn tako, wait for it to exit, return the rendered screen text and
 * the full TakoTerminal for deeper inspection.
 */
export async function run(
  args: string[],
  opts: Omit<SpawnOptions, "args"> = {},
): Promise<{ term: TakoTerminal; screen: string; exitCode: number }> {
  const term = TakoTerminal.spawn({ ...opts, args });
  const exitCode = await term.waitForExit({
    timeout: opts.timeout ?? DEFAULT_TIMEOUT_MS,
  });
  // Give xterm.js a tick to finish processing
  await sleep(100);
  return { term, screen: term.screenText(), exitCode };
}
