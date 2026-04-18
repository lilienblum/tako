#!/usr/bin/env bun
// Emits .md twins alongside each .html file in dist/ so the Worker can serve
// raw markdown when agents request Accept: text/markdown.
//
// Route-driven: walks the Astro build output, picks the most specific content
// container per page type, strips chrome/decorative elements, converts to
// markdown, and writes a sibling .md.

import { readdir, readFile, writeFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parse, type HTMLElement } from "node-html-parser";
import { NodeHtmlMarkdown } from "node-html-markdown";

const ROOT = path.resolve(fileURLToPath(new URL(".", import.meta.url)), "..");
const DIST = path.join(ROOT, "dist");
const SITE = "https://tako.sh";

const CONTENT_SELECTORS = ["article.docs-main .content", "article.blog-article", "main.container"];

const STRIP_SELECTORS = [
  "script",
  "style",
  "noscript",
  "link",
  "svg",
  "template",
  "nav",
  "aside",
  ".breadcrumb",
  ".docs-menu-toggle",
  ".copy-inline",
  ".copy-button",
  ".panel-hanger",
  ".cursor",
  ".hero-block-rule",
  ".feature-show-all",
  ".included-tags",
  ".command-card",
];

const converter = new NodeHtmlMarkdown({
  bulletMarker: "-",
  codeBlockStyle: "fenced",
  useInlineLinks: true,
  keepDataImages: false,
});

async function walk(dir: string): Promise<string[]> {
  const entries = await readdir(dir, { withFileTypes: true });
  const files: string[] = [];
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await walk(full)));
    } else {
      files.push(full);
    }
  }
  return files;
}

function distRelToRoute(rel: string): string {
  return rel.replace(/\/?index\.html$/, "").replace(/\.html$/, "");
}

function canonicalFor(route: string): string {
  return route === "" ? `${SITE}/` : `${SITE}/${route}/`;
}

function selectContent(root: HTMLElement): HTMLElement | null {
  for (const selector of CONTENT_SELECTORS) {
    const el = root.querySelector(selector);
    if (el) return el;
  }
  return null;
}

function yamlEscape(value: string): string {
  return value.replaceAll('"', '\\"');
}

function buildFrontmatter(root: HTMLElement, canonical: string): string {
  const title = root.querySelector("title")?.text?.trim() ?? "";
  const description =
    root.querySelector('meta[name="description"]')?.getAttribute("content")?.trim() ?? "";
  const lines = ["---"];
  if (title) lines.push(`title: "${yamlEscape(title)}"`);
  if (description) lines.push(`description: "${yamlEscape(description)}"`);
  lines.push(`canonical: "${canonical}"`);
  lines.push("---", "");
  return lines.join("\n");
}

function htmlToMarkdown(html: string, canonical: string): string | null {
  const root = parse(html);
  const content = selectContent(root);
  if (!content) return null;
  for (const selector of STRIP_SELECTORS) {
    for (const node of content.querySelectorAll(selector)) node.remove();
  }
  const body = converter.translate(content.innerHTML).trim();
  if (!body) return null;
  return `${buildFrontmatter(root, canonical)}${body}\n`;
}

async function main() {
  const distStat = await stat(DIST).catch(() => null);
  if (!distStat?.isDirectory()) {
    console.error(`emit-markdown: dist not found at ${DIST} (run astro build first)`);
    process.exit(1);
  }

  const htmlFiles = (await walk(DIST)).filter((f) => f.endsWith(".html"));
  const skipped: string[] = [];
  let emitted = 0;

  for (const htmlPath of htmlFiles) {
    const rel = path.relative(DIST, htmlPath).replaceAll(path.sep, "/");
    const route = distRelToRoute(rel);
    const html = await readFile(htmlPath, "utf8");
    const markdown = htmlToMarkdown(html, canonicalFor(route));
    if (!markdown) {
      skipped.push(rel);
      continue;
    }
    const mdOut = htmlPath.replace(/\.html$/, ".md");
    await writeFile(mdOut, markdown);
    emitted++;
  }

  console.log(`emit-markdown: wrote ${emitted} markdown file(s)`);
  if (skipped.length > 0) {
    console.log(`emit-markdown: skipped ${skipped.length} html without content container:`);
    for (const rel of skipped) console.log(`  - ${rel}`);
  }
}

await main();
