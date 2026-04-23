import broadcast from "./workflows/broadcast";

export default async function fetch(request: Request): Promise<Response> {
  const url = new URL(request.url);

  if (url.pathname === "/" && request.method === "GET") {
    return new Response("<!doctype html><html><body><h1>Tako app</h1></body></html>", {
      headers: { "content-type": "text/html; charset=utf-8" },
    });
  }

  if (url.pathname === "/enqueue" && request.method === "POST") {
    const { message } = (await request.json()) as { message: string };
    const runId = await broadcast.enqueue({ message });
    return Response.json({ ok: true, runId });
  }

  return new Response("Not Found", { status: 404 });
}
