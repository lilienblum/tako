export default async function fetch(request: Request) {
  const url = new URL(request.url);
  if (url.pathname === "/") {
    return new Response("<!doctype html><html><body><h1>Tako app</h1></body></html>", {
      headers: {
        "content-type": "text/html; charset=utf-8",
      },
    });
  }

  return new Response("Not Found", { status: 404 });
}
