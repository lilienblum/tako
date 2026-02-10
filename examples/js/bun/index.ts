export default {
  async fetch(req: Request) {
    const url = new URL(req.url);

    if (url.pathname === "/") {
			return Response.json({
				success: "ok",
				respondedAt: new Date().toISOString()
			}, { headers: { "content-type": "application/json; charset=utf-8" } });
    }

    return new Response("Not Found", { status: 404 })
  },
};
