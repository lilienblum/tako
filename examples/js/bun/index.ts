export default async function fetch(req: Request) {
  const url = new URL(req.url);
  const tenant = getTenantFromHostname(url.hostname);
  const pid = process.pid;

  if (url.pathname === "/bun" || url.pathname === "/bun/") {
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
  const parts = hostname.split(".").filter(Boolean);
  if (parts.length < 2) {
    return null;
  }

  return parts[0] ?? null;
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}
