import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

describe("package exports", () => {
  test("declares the vite export from dist output", () => {
    const packageJson = JSON.parse(
      readFileSync(resolve(import.meta.dirname, "..", "package.json"), "utf8"),
    );

    expect(packageJson.exports["./vite"]).toEqual({
      types: "./dist/vite.d.mts",
      import: "./dist/vite.mjs",
    });
  });

  test("declares the Next.js export from dist output", () => {
    const packageJson = JSON.parse(
      readFileSync(resolve(import.meta.dirname, "..", "package.json"), "utf8"),
    );

    expect(packageJson.exports["./nextjs"]).toEqual({
      types: "./dist/nextjs.d.mts",
      import: "./dist/nextjs.mjs",
    });
  });
});
