const port = Number(process.env.PORT || 3000);

const app = {
  hostname: "0.0.0.0",
  port,
  async fetch(req: Request) {
    const url = new URL(req.url);
    const host = req.headers.get("host") ?? url.host;

    if (url.pathname === "/_tako/status" || url.pathname === "/_tako/health") {
      return Response.json(
        {
          status: "ok",
          host,
          respondedAt: new Date().toISOString(),
        },
        { headers: { "content-type": "application/json; charset=utf-8" } },
      );
    }

    if (url.pathname === "/") {
      return Response.json(
        {
          success: "ok",
          host,
          respondedAt: new Date().toISOString(),
        },
        { headers: { "content-type": "application/json; charset=utf-8" } },
      );
    }

    return new Response("Not Found", { status: 404 });
  },
};

console.log(`bun-example listening on 0.0.0.0:${port}`);

export default app;
