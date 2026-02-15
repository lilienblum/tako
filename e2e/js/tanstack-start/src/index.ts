const port = Number(process.env.PORT ?? 3000);

export default {
  hostname: "0.0.0.0",
  port,
  fetch(request: Request) {
    const url = new URL(request.url);

    if (url.pathname === "/_tako/status" || url.pathname === "/_tako/health") {
      return Response.json({
        status: "ok",
        app: "tanstack-start-e2e",
      });
    }

    return new Response("tanstack-start e2e source entry");
  },
};
