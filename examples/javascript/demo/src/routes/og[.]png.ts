import { createFileRoute } from "@tanstack/react-router";

import { renderOgPng } from "@/lib/og";
import { parseTenant } from "@/lib/host";

export const Route = createFileRoute("/og.png")({
  server: {
    handlers: {
      GET: async ({ request }) => {
        const host = request.headers.get("host") ?? "";
        const url = new URL(request.url);
        const override = url.searchParams.get("base");
        const tenantSlug = override ?? parseTenant(host);
        const png = await renderOgPng({ tenantSlug });
        return new Response(new Uint8Array(png), {
          headers: {
            "Content-Type": "image/png",
            "Cache-Control": "public, max-age=3600, s-maxage=86400",
          },
        });
      },
    },
  },
});
