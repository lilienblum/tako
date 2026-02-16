export default async function fetch(req: Request) {
  const url = new URL(req.url);
  const host = req.headers.get("host") ?? url.host;

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
}
