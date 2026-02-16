export default async function fetch(request: Request) {
  const url = new URL(request.url);
  const host = request.headers.get("host") ?? url.host;

  return Response.json({
    status: "ok",
    app: "bun-e2e",
    host,
    pathname: url.pathname,
  });
}
