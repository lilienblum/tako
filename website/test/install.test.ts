import { describe, expect, it } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

describe("installer scripts", () => {
  it("CLI installer script exists in scripts/ and is POSIX sh compatible", async () => {
    const scriptPath = join(
      import.meta.dir,
      "..",
      "..",
      "scripts",
      "install-tako-cli.sh",
    );
    const s = await readFile(scriptPath, "utf8");

    expect(s.startsWith("#!/bin/sh")).toBe(true);
    expect(s).toContain("set -eu");
    expect(s).toContain("TAKO_INSTALL_DIR");
    expect(s).toContain("OK installed tako");
    expect(s).not.toContain("[[");
    expect(s).not.toContain("pipefail");
  });

  it("server installer script exists in scripts/ and is POSIX sh compatible", async () => {
    const scriptPath = join(
      import.meta.dir,
      "..",
      "..",
      "scripts",
      "install-tako-server.sh",
    );
    const s = await readFile(scriptPath, "utf8");

    expect(s.startsWith("#!/bin/sh")).toBe(true);
    expect(s).toContain("set -eu");
    expect(s).toContain("TAKO_USER");
    expect(s).toContain("systemctl");
    expect(s).toContain("setcap cap_net_bind_service=+ep");
    expect(s).not.toContain("[[");
    expect(s).not.toContain("pipefail");
  });
});
