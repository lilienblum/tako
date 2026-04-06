import rss from "@astrojs/rss";
import { getCollection } from "astro:content";
import type { APIContext } from "astro";

export async function GET(context: APIContext) {
  const posts = await getCollection("blog");

  const items = posts
    .filter((p) => p.data.title && p.data.date)
    .sort((a, b) => new Date(b.data.date).getTime() - new Date(a.data.date).getTime())
    .map((p) => ({
      title: p.data.title,
      pubDate: new Date(p.data.date),
      description: p.data.description ?? "",
      link: `/blog/${p.id}/`,
    }));

  return rss({
    title: "Tako Blog",
    description: "Updates, ideas, and progress from the Tako project.",
    site: context.site!,
    items,
  });
}
