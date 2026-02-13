import { defineCollection, z } from "astro:content";

const docs = defineCollection({
  type: "content",
  schema: z.object({}).passthrough(),
});

export const collections = { docs };
