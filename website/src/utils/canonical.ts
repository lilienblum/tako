export function normalizeCanonicalPath(pathname: string): string {
  if (pathname === "/" || pathname === "/index.html") {
    return "/";
  }

  if (pathname.endsWith("/index.html")) {
    return pathname.slice(0, -"/index.html".length) || "/";
  }

  if (pathname.endsWith(".html")) {
    return pathname.slice(0, -".html".length) || "/";
  }

  if (pathname.length > 1 && pathname.endsWith("/")) {
    return pathname.slice(0, -1);
  }

  return pathname;
}

export function createCanonicalUrl(pathname: string, site: URL | undefined): URL {
  return new URL(normalizeCanonicalPath(pathname), site);
}
