import { getCollection } from "astro:content";
import type { APIContext } from "astro";
import { llmsCoreSections, renderLlmsTxt, type LlmsSection } from "../utils/llms";

export async function GET(context: APIContext) {
  const posts = await getCollection("blog");
  const site = context.site ?? new URL("https://tako.sh");

  const coreSections = llmsCoreSections.map((section) => ({
    ...section,
    links: section.links.map((link) => ({
      ...link,
      url: new URL(link.url, site).toString(),
    })),
  }));

  const optionalLinks = posts
    .filter((post) => post.data.title && post.data.date)
    .sort((a, b) => new Date(b.data.date).getTime() - new Date(a.data.date).getTime())
    .map((post) => ({
      title: post.data.title,
      url: new URL(`/blog/${post.id}/`, site).toString(),
      description: post.data.description,
    }));

  const sections: LlmsSection[] =
    optionalLinks.length > 0
      ? [...coreSections, { title: "Optional", links: optionalLinks }]
      : coreSections;

  return new Response(renderLlmsTxt("Tako", sections), {
    headers: {
      "Content-Type": "text/plain; charset=utf-8",
    },
  });
}
