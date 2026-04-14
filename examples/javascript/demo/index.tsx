const WIDLCARD_SUFFIX = process.env.WIDLCARD_SUFFIX!;

export default function fetch(req: Request) {
  const url = new URL(req.url);
  const tenant = getTenantFromHostname(url.hostname);
  const servesRootForTenant = tenant !== null && (url.pathname === "/" || url.pathname === "");

  if (url.pathname === "/foobar" || url.pathname === "/foobar/" || servesRootForTenant) {
    const body = (
      <>
        <h1>Bun example for tako</h1>
        {tenant && <p>Tenant: {tenant}</p>}
        <p>PID: {process.pid}</p>
      </>
    );
    return new Response(body.html, {
      headers: { "content-type": "text/html; charset=utf-8" },
    });
  }

  return new Response("Not Found", { status: 404 });
}

function getTenantFromHostname(hostname: string): string | null {
  if (!hostname.endsWith(WIDLCARD_SUFFIX)) return null;
  const tenant = hostname.slice(0, -WIDLCARD_SUFFIX.length);
  return tenant.length > 0 ? tenant : null;
}

// Minimal JSX-to-HTML runtime. No React dependency: each element produces
// an `Html` marker so pre-rendered fragments skip re-escaping when they
// appear as children of another element.
type Child = string | number | boolean | null | undefined | Html;

class Html {
  constructor(readonly html: string) {}
}

// eslint-disable-next-line @typescript-eslint/no-empty-object-type
type Props = {} | null | undefined;
type Component = (props: Props, ...children: Array<Child | Child[]>) => Html;

function h(tag: string | Component, props: Props, ...children: Array<Child | Child[]>): Html {
  if (typeof tag === "function") return tag(props, ...children);
  const attrs = props
    ? Object.entries(props)
        .filter(([, v]) => v !== false && v != null)
        .map(([k, v]) => ` ${k}="${escapeHtml(String(v))}"`)
        .join("")
    : "";
  return new Html(`<${tag}${attrs}>${renderChildren(children)}</${tag}>`);
}

function Fragment(_props: Props, ...children: Array<Child | Child[]>): Html {
  return new Html(renderChildren(children));
}

function renderChildren(children: Array<Child | Child[]>): string {
  const flat: Child[] = [];
  for (const c of children) {
    if (Array.isArray(c)) flat.push(...c);
    else flat.push(c);
  }
  return flat
    .filter(
      (c): c is Exclude<Child, null | undefined | false | true> =>
        c !== null && c !== undefined && c !== false && c !== true,
    )
    .map((c) => (c instanceof Html ? c.html : escapeHtml(String(c))))
    .join("");
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

declare global {
  namespace JSX {
    type Element = Html;
    interface IntrinsicElements {
      [elemName: string]: Props;
    }
  }
}
