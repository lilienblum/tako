import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import type { Server, Socket } from "node:net";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { WorkflowsClient } from "../../src/workflows/rpc-client";
import type { Run } from "../../src/workflows/types";
import type { RegisteredWorkflow, WorkflowHandler } from "../../src/workflows/worker";
import { Worker } from "../../src/workflows/worker";

class MockServer {
  server!: Server;
  path = "";
  private tasks: Run[] = [];
  private idCounter = 0;

  async start(dir: string): Promise<void> {
    this.path = join(dir, "srv.sock");
    this.server = createServer((socket: Socket) => this.handleConnection(socket));
    await new Promise<void>((r) => this.server.listen(this.path, r));
  }

  async close(): Promise<void> {
    await new Promise<void>((r) => this.server.close(() => r()));
  }

  seed(task: Partial<Run> & { name: string }): string {
    const id = `t${++this.idCounter}`;
    this.tasks.push({
      id,
      name: task.name,
      payload: task.payload ?? {},
      status: "pending",
      attempts: 0,
      retries: task.retries ?? 2,
      runAt: task.runAt ?? Date.now(),
      leaseUntil: null,
      workerId: null,
      lastError: null,
      stepState: task.stepState ?? {},
      createdAt: Date.now(),
      uniqueKey: null,
    });
    return id;
  }

  find(id: string): Run | undefined {
    return this.tasks.find((t) => t.id === id);
  }

  private handleConnection(socket: Socket): void {
    let buf = "";
    socket.on("data", (chunk: Buffer) => {
      buf += chunk.toString("utf8");
      let nl: number;
      while ((nl = buf.indexOf("\n")) !== -1) {
        const line = buf.slice(0, nl);
        buf = buf.slice(nl + 1);
        try {
          const cmd = JSON.parse(line) as Record<string, unknown>;
          const resp = this.dispatch(cmd);
          socket.write(`${JSON.stringify(resp)}\n`);
        } catch (err) {
          socket.write(`${JSON.stringify({ status: "error", message: String(err) })}\n`);
        }
      }
    });
  }

  private dispatch(cmd: Record<string, unknown>): unknown {
    switch (cmd["command"]) {
      case "claim_run": {
        const names = cmd["names"] as string[];
        const task = this.tasks.find(
          (t) => t.status === "pending" && names.includes(t.name) && t.runAt <= Date.now(),
        );
        if (!task) return { status: "ok", data: null };
        task.status = "running";
        task.attempts += 1;
        task.workerId = cmd["worker_id"] as string;
        return {
          status: "ok",
          data: {
            id: task.id,
            name: task.name,
            payload: task.payload,
            status: task.status,
            attempts: task.attempts,
            max_attempts: task.retries + 1,
            run_at_ms: task.runAt,
            step_state: task.stepState,
          },
        };
      }
      case "heartbeat_run":
        return { status: "ok", data: {} };
      case "save_step": {
        const task = this.find(cmd["id"] as string);
        if (task) {
          // Steps table model: append (step_name, result) per call.
          const stepName = cmd["step_name"] as string;
          task.stepState = { ...task.stepState, [stepName]: cmd["result"] };
        }
        return { status: "ok", data: {} };
      }
      case "complete_run": {
        const task = this.find(cmd["id"] as string);
        if (task) {
          task.status = "succeeded";
          task.workerId = null;
        }
        return { status: "ok", data: {} };
      }
      case "cancel_run": {
        const task = this.find(cmd["id"] as string);
        if (task) {
          task.status = "cancelled";
          task.lastError = (cmd["reason"] as string) ?? null;
          task.workerId = null;
        }
        return { status: "ok", data: {} };
      }
      case "defer_run": {
        const task = this.find(cmd["id"] as string);
        if (task) {
          task.status = "pending";
          task.runAt = (cmd["wake_at_ms"] as number) ?? Number.MAX_SAFE_INTEGER;
          task.workerId = null;
        }
        return { status: "ok", data: {} };
      }
      case "wait_for_event": {
        const task = this.find(cmd["id"] as string);
        if (task) {
          task.status = "pending";
          task.runAt = (cmd["timeout_at_ms"] as number) ?? Number.MAX_SAFE_INTEGER;
          task.workerId = null;
        }
        return { status: "ok", data: {} };
      }
      case "fail_run": {
        const task = this.find(cmd["id"] as string);
        if (task) {
          if (cmd["finalize"]) {
            task.status = "dead";
          } else {
            task.status = "pending";
            task.runAt = (cmd["next_run_at_ms"] as number) ?? Date.now();
          }
          task.lastError = cmd["error"] as string;
          task.workerId = null;
        }
        return { status: "ok", data: {} };
      }
      default:
        return { status: "error", message: `unknown: ${String(cmd["command"])}` };
    }
  }
}

function registry(handlers: Record<string, WorkflowHandler>): Map<string, RegisteredWorkflow> {
  return new Map(Object.entries(handlers).map(([name, handler]) => [name, { handler }]));
}

describe("Worker", () => {
  let dir: string;
  let mock: MockServer;
  let client: WorkflowsClient;

  beforeEach(async () => {
    dir = await mkdtemp(join("/tmp", "tako-worker-"));
    mock = new MockServer();
    await mock.start(dir);
    client = new WorkflowsClient(mock.path);
  });

  afterEach(async () => {
    await mock.close();
    await rm(dir, { recursive: true, force: true });
  });

  test("processes one task and marks it succeeded", async () => {
    const seen: unknown[] = [];
    const worker = new Worker({
      client,
      workerId: "w1",
      registry: registry({
        echo: (p) => {
          seen.push(p);
          return "ok";
        },
      }),
    });

    const id = mock.seed({ name: "echo", payload: { hello: 1 } });
    expect(await worker.processOnce()).toBe(true);
    expect(seen).toEqual([{ hello: 1 }]);
    expect(mock.find(id)?.status).toBe("succeeded");
  });

  test("processOnce returns false when nothing is eligible", async () => {
    const worker = new Worker({ client, workerId: "w1", registry: registry({}) });
    expect(await worker.processOnce()).toBe(false);
  });

  test("failing handler exhausts retries and dies", async () => {
    const worker = new Worker({
      client,
      workerId: "w1",
      baseBackoffMs: 1,
      maxBackoffMs: 2,
      registry: registry({
        flaky: () => {
          throw new Error("boom");
        },
      }),
    });

    const id = mock.seed({ name: "flaky", retries: 1 });
    await worker.processOnce();
    expect(mock.find(id)?.status).toBe("pending");
    expect(mock.find(id)?.attempts).toBe(1);

    await new Promise((r) => setTimeout(r, 10));
    await worker.processOnce();
    expect(mock.find(id)?.status).toBe("dead");
  });

  test("step.run memoizes across retries", async () => {
    const runs: Record<string, number> = { a: 0, b: 0 };
    let forceFail = true;
    // eslint-disable-next-line @typescript-eslint/unbound-method -- `run` is a bound closure, not a class method
    const handler: WorkflowHandler = async (_payload, { run }) => {
      const v = await run("a", () => {
        runs.a += 1;
        return "user-1";
      });
      await run("b", () => {
        runs.b += 1;
        if (forceFail) throw new Error("fail-b");
        return v;
      });
    };

    const worker = new Worker({
      client,
      workerId: "w1",
      baseBackoffMs: 1,
      maxBackoffMs: 2,
      registry: registry({ multi: handler }),
    });

    const id = mock.seed({ name: "multi", retries: 4 });
    await worker.processOnce();
    expect(mock.find(id)?.status).toBe("pending");
    expect(mock.find(id)?.stepState).toEqual({ a: "user-1" });

    forceFail = false;
    await new Promise((r) => setTimeout(r, 10));
    await worker.processOnce();
    expect(mock.find(id)?.status).toBe("succeeded");
    expect(runs.a).toBe(1);
    expect(runs.b).toBe(2);
  });
});
