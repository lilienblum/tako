import rss from "@astrojs/rss";
import type { APIContext } from "astro";

export async function GET(context: APIContext) {
  const posts = import.meta.glob<{
    frontmatter: Record<string, string>;
    url: string;
  }>("./blog/*.md", { eager: true });

  const items = Object.values(posts)
    .filter((p) => p.frontmatter.title && p.frontmatter.date)
    .sort((a, b) => new Date(b.frontmatter.date).getTime() - new Date(a.frontmatter.date).getTime())
    .map((p) => ({
      title: p.frontmatter.title,
      pubDate: new Date(p.frontmatter.date),
      description: p.frontmatter.description ?? "",
      link: p.url!,
    }));

  return rss({
    title: "Tako Blog",
    description: "Updates, ideas, and progress from the Tako project.",
    site: context.site!,
    items,
  });
}
