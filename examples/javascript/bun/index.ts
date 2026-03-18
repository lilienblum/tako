const WIDLCARD_SUFFIX = process.env.WIDLCARD_SUFFIX!;

export default async function fetch(req: Request) {
  const url = new URL(req.url);
  const tenant = getTenantFromHostname(url.hostname);
  const pid = process.pid;
  const servesRootForTenant = tenant !== null && (url.pathname === "/" || url.pathname === "");

  if (url.pathname === "/bun" || url.pathname === "/bun/" || servesRootForTenant) {
    const tenantHtml = tenant ? `<p>Tenant: ${escapeHtml(tenant)}</p>` : "";
    const pidHtml = `<p>PID: ${pid}</p>`;
    const html = `<h1>Bun example for tako</h1>${tenantHtml}${pidHtml}`;

    return new Response(html, {
      headers: {
        "content-type": "text/html; charset=utf-8",
      },
    });
  }

  return new Response("Not Found", { status: 404 });
}

function getTenantFromHostname(hostname: string): string | null {
  if (!hostname.endsWith(WIDLCARD_SUFFIX)) {
    return null;
  }

  const tenant = hostname.slice(0, -WIDLCARD_SUFFIX.length);
  return tenant.length > 0 ? tenant : null;
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}
