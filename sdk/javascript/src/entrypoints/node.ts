#!/usr/bin/env node
/**
 * Tako Node.js Entrypoint — run via `npx tako-node <main>`
 */

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { Readable } from "node:stream";
import { createEntrypoint } from "../create-entrypoint";

const { run, appSocketPath, port, setDraining } = createEntrypoint();

function incomingMessageToRequest(req: IncomingMessage): Request {
  const url = new URL(req.url || "/", `http://${req.headers.host || "localhost"}`);
  const method = req.method || "GET";
  const headers = new Headers();
  for (const [key, value] of Object.entries(req.headers)) {
    if (value === undefined) continue;
    if (Array.isArray(value)) {
      for (const v of value) headers.append(key, v);
    } else {
      headers.set(key, value);
    }
  }

  const hasBody = method !== "GET" && method !== "HEAD";
  const body = hasBody
    ? new ReadableStream({
        start(controller) {
          req.on("data", (chunk: Buffer) => controller.enqueue(chunk));
          req.on("end", () => controller.close());
          req.on("error", (err) => controller.error(err));
        },
      })
    : null;

  return new Request(url.href, { method, headers, body, duplex: "half" } as RequestInit);
}

async function writeResponse(webResponse: Response, res: ServerResponse): Promise<void> {
  const headers: Record<string, string | string[]> = {};
  webResponse.headers.forEach((value, key) => {
    const existing = headers[key];
    if (existing !== undefined) {
      headers[key] = Array.isArray(existing) ? [...existing, value] : [existing, value];
    } else {
      headers[key] = value;
    }
  });
  res.writeHead(webResponse.status, headers);

  if (!webResponse.body) {
    res.end();
    return;
  }

  const nodeStream = Readable.fromWeb(
    webResponse.body as unknown as import("node:stream/web").ReadableStream,
  );
  nodeStream.pipe(res);
  await new Promise<void>((resolve, reject) => {
    nodeStream.on("end", resolve);
    nodeStream.on("error", reject);
  });
}

void run((handleRequest) => {
  const server = createServer(async (req, res) => {
    try {
      const request = incomingMessageToRequest(req);
      const response = await handleRequest(request);
      await writeResponse(response, res);
    } catch (err) {
      console.error("Error handling request:", err);
      if (!res.headersSent) {
        res.writeHead(500, { "Content-Type": "application/json" });
      }
      res.end(JSON.stringify({ error: "Internal Server Error" }));
    }
  });

  if (appSocketPath) {
    server.listen(appSocketPath, () => {
      console.log(`Application listening on ${appSocketPath}`);
    });
  } else {
    server.listen(port, () => {
      console.log(`Application listening on http://localhost:${port}`);
    });
  }

  process.on("SIGTERM", () => {
    setDraining();
  });
});
