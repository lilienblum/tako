const port = Number(process.env.PORT ?? 3000);

export default {
  hostname: "0.0.0.0",
  port,
  fetch(request: Request) {
    const url = new URL(request.url);

    if (url.pathname === "/_tako/status" || url.pathname === "/_tako/health") {
      return Response.json({
        status: "ok",
        app: "tanstack-start-example-basic",
        mode: "tako-runtime-entry",
      });
    }

    return new Response(
      "TanStack Start example: run `bun run dev` for local dev or `bun run build` to stage .tako/artifacts/app.",
    );
  },
};
