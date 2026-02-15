const port = Number(process.env.PORT ?? 3000);

function contentTypeFor(pathname: string): string {
  if (pathname.endsWith(".html")) return "text/html; charset=utf-8";
  if (pathname.endsWith(".css")) return "text/css; charset=utf-8";
  if (pathname.endsWith(".js") || pathname.endsWith(".mjs")) {
    return "application/javascript; charset=utf-8";
  }
  if (pathname.endsWith(".json")) return "application/json; charset=utf-8";
  if (pathname.endsWith(".txt")) return "text/plain; charset=utf-8";
  return "application/octet-stream";
}

export default {
  hostname: "0.0.0.0",
  port,
  async fetch(request: Request) {
    const url = new URL(request.url);

    if (url.pathname === "/_tako/status" || url.pathname === "/_tako/health") {
      return Response.json({ status: "ok", app: "tanstack-start-e2e-release" });
    }

    if (url.pathname === "/") {
      const indexFile = Bun.file("./static/index.html");
      if (await indexFile.exists()) {
        return new Response(indexFile, {
          headers: { "content-type": "text/html; charset=utf-8" },
        });
      }
    }

    if (url.pathname.startsWith("/static/")) {
      const assetFile = Bun.file(`.${url.pathname}`);
      if (await assetFile.exists()) {
        return new Response(assetFile, {
          headers: { "content-type": contentTypeFor(url.pathname) },
        });
      }
    }

    return new Response("Not Found", { status: 404 });
  },
};
