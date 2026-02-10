export interface Env {
  ASSETS: Fetcher;
}

const INSTALL_SCRIPT_PATHS = new Set(["/install", "/install-server", "/server-install"]);

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    // Ensure curl-friendly headers for installer scripts.
    if (INSTALL_SCRIPT_PATHS.has(url.pathname)) {
      const assetRequest =
        url.pathname === "/server-install"
          ? new Request(new URL("/install-server", url), request)
          : request;
      const res = await env.ASSETS.fetch(assetRequest);
      const headers = new Headers(res.headers);
      headers.set("content-type", "text/plain; charset=utf-8");
      return new Response(res.body, { status: res.status, headers });
    }

    return env.ASSETS.fetch(request);
  },
};
