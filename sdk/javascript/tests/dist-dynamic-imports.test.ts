import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const DIST_DIR = resolve(import.meta.dirname, "..", "dist");

// Vite's `vite:import-analysis` plugin scans every module it ingests
// (including externalized SDK dist chunks during dep pre-bundling) for
// `await import(<non-literal>)` calls and emits a warning when it
// cannot statically resolve the specifier. Authors normally suppress
// the warning with `/* @vite-ignore */`, but tsdown's minifier strips
// the annotation comment, so any bundled SDK chunk that still emits a
// literal `import(...)` call with a non-string argument will trip the
// warning in every downstream Vite consumer.
//
// The SDK's `workflows/discovery.ts`, `channels/discovery.ts`, and
// `tako/create-entrypoint.ts` each need a truly-dynamic import by url.
// They dodge Vite's analyzer by going through a Function-constructor
// indirection, so no literal `import(...)` appears in the bundled
// output at all.
describe("bundled SDK chunks", () => {
  test("do not emit a statically-unanalyzable `import(<expr>)` that would trip vite:import-analysis", () => {
    const offenders: string[] = [];
    for (const file of readdirSync(DIST_DIR)) {
      if (!file.endsWith(".mjs")) continue;
      const content = readFileSync(join(DIST_DIR, file), "utf8");
      // Vite uses a real JS parser, so it ignores `import(...)` that
      // appears inside string / template-literal bodies. Strip those
      // before scanning so we only match real call expressions.
      const stripped = content
        .replace(/`(?:\\.|[^`\\])*`/g, "``")
        .replace(/"(?:\\.|[^"\\])*"/g, '""')
        .replace(/'(?:\\.|[^'\\])*'/g, "''");
      // Match `import(` whose first argument is neither a string literal
      // nor whitespace — i.e. a non-literal specifier Vite can't resolve.
      const matches = stripped.match(/\bimport\([^'"`\s)]/g);
      if (matches && matches.length > 0) {
        offenders.push(`${file}: ${matches.length} occurrence(s)`);
      }
    }
    expect(offenders).toEqual([]);
  });
});
