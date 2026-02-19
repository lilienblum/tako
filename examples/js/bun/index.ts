export default async function fetch(req: Request) {
  const url = new URL(req.url);

  if (url.pathname === "/bun" || url.pathname === "/bun/") {
    return new Response("<h1>Bun example for tako</h1>", {
      headers: {
        "content-type": "text/html; charset=utf-8",
      },
    });
  }

  return new Response("Not Found", { status: 404 });
}
