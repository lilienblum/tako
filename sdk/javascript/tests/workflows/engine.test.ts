import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import type { Server } from "node:net";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { WorkflowsClient } from "../../src/workflows/rpc-client";
import { WorkflowEngine } from "../../src/workflows/engine";

function startStubServer(path: string, resp: unknown): Promise<Server> {
  return new Promise((resolve, reject) => {
    const server = createServer((socket) => {
      socket.on("data", () => {
        socket.write(`${JSON.stringify(resp)}\n`);
      });
    });
    server.once("error", reject);
    server.listen(path, () => resolve(server));
  });
}

describe("WorkflowEngine registration", () => {
  test("duplicate register throws", () => {
    const engine = new WorkflowEngine();
    engine.register("w", () => {});
    expect(() => engine.register("w", () => {})).toThrow(/already registered/);
  });

  test("registeredNames reflects registrations", () => {
    const engine = new WorkflowEngine();
    engine.register("a", () => {});
    engine.register("b", () => {});
    expect(engine.registeredNames.sort()).toEqual(["a", "b"]);
  });

  test("collectSchedules returns workflows with a schedule config", () => {
    const engine = new WorkflowEngine();
    engine.register("daily", () => {}, { schedule: "0 0 * * * *" });
    engine.register("nocron", () => {});
    expect(engine.collectSchedules()).toEqual([{ name: "daily", cron: "0 0 * * * *" }]);
  });
});

describe("WorkflowEngine enqueue (RPC)", () => {
  let dir: string;
  let sock: string;
  let server: Server | undefined;

  beforeEach(async () => {
    dir = await mkdtemp(join("/tmp", "tako-engine-"));
    sock = join(dir, "srv.sock");
  });

  afterEach(async () => {
    server?.close();
    server = undefined;
    await rm(dir, { recursive: true, force: true });
  });

  test("throws when no RPC client is configured or discoverable", async () => {
    const engine = new WorkflowEngine();
    const prev = process.env["TAKO_ENQUEUE_SOCKET"];
    delete process.env["TAKO_ENQUEUE_SOCKET"];
    try {
      await expect(engine.enqueue("w", {})).rejects.toThrow(/RPC client/);
    } finally {
      if (prev !== undefined) process.env["TAKO_ENQUEUE_SOCKET"] = prev;
    }
  });

  test("delegates to the configured client", async () => {
    server = await startStubServer(sock, {
      status: "ok",
      data: { id: "srv-1", deduplicated: false },
    });
    const engine = new WorkflowEngine();
    engine.setClient(new WorkflowsClient(sock));
    expect(await engine.enqueue("w", { hi: 1 })).toBe("srv-1");
  });

  test("applies per-workflow maxAttempts default when caller omits it", async () => {
    let received: Record<string, unknown> | null = null;
    server = await new Promise<Server>((resolve, reject) => {
      const s = createServer((socket) => {
        socket.on("data", (chunk: Buffer) => {
          received = JSON.parse(chunk.toString().trim()) as Record<string, unknown>;
          socket.write(
            `${JSON.stringify({ status: "ok", data: { id: "x", deduplicated: false } })}\n`,
          );
        });
      });
      s.once("error", reject);
      s.listen(sock, () => resolve(s));
    });

    const engine = new WorkflowEngine();
    engine.setClient(new WorkflowsClient(sock));
    engine.register("w", () => {}, { maxAttempts: 7 });
    await engine.enqueue("w", {});
    const opts = (received as unknown as Record<string, Record<string, unknown>>)["opts"];
    expect(opts["max_attempts"]).toBe(7);
  });
});

describe("discover", () => {
  test("discovers default exports and named config", async () => {
    const dir = await mkdtemp(join("/tmp", "tako-wf-"));
    await writeFile(
      join(dir, "send-email.mjs"),
      `export default async (ctx, p) => p.to;\nexport const maxAttempts = 5;\nexport const schedule = "*/5 * * * *";`,
    );
    await writeFile(join(dir, "bare.mjs"), `export default function(ctx, p) { return "ok"; }`);
    await writeFile(join(dir, "_ignored.mjs"), `export default () => {};`);

    const engine = new WorkflowEngine();
    const count = await engine.discover(dir);
    expect(count).toBe(2);
    expect(engine.registeredNames.sort()).toEqual(["bare", "send-email"]);
  });

  test("missing directory returns 0 and does not throw", async () => {
    const engine = new WorkflowEngine();
    const count = await engine.discover(join("/tmp", "tako-nonexistent-" + Date.now()));
    expect(count).toBe(0);
  });

  test("rejects files without a default function export", async () => {
    const dir = await mkdtemp(join("/tmp", "tako-wf-"));
    await writeFile(join(dir, "bad.mjs"), `export const foo = 1;`);
    const engine = new WorkflowEngine();
    await expect(engine.discover(dir)).rejects.toThrow(/default-export a function/);
  });
});
