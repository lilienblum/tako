# Blog Post

Create a new blog post for the Tako website based on an idea provided by the user.

## Input

The user provides a topic or idea. Examples:

- "write about why we don't use Docker"
- "compare Tako to Kamal"
- "announce the new secrets feature"

## Process

### Step 1 — Research

1. Read `SPEC.md` to understand current Tako capabilities and architecture.
2. Read existing blog posts in `website/src/pages/blog/` to match tone and style.
3. Check memory for competitor landscape data (reference_competitor_landscape.md) for context on similar tools.
4. If the topic involves a specific feature, read the relevant source code to get details right.
5. If the topic involves competitors or external tools, do web research to get current facts (stars, versions, status).
6. **Fact-check rigorously.** Every factual claim (star counts, release dates, feature support, version numbers) must be verified against at least two independent sources. Cross-reference docs, GitHub, and web search results. If two sources disagree, dig until you find the truth. Do not publish a number you only saw once.

### Step 2 — Write

Create a new markdown file at `website/src/pages/blog/{slug}.md` with this frontmatter:

```markdown
---
layout: ../../layouts/BlogPostLayout.astro
title: "Post Title"
date: "YYYY-MM-DDTHH:MM"
description: "A concise 1-2 sentence summary for SEO meta tags and social previews. Should be compelling and specific — not generic."
image: 9q15scNA
---
```

The `description` field is **required** — it populates `<meta name="description">`, Open Graph, Twitter cards, and the blog listing page. Write it as a standalone sentence that makes sense in search results and social shares. Keep it under 160 characters.

Hero images go in `website/public/assets/blog/` as `.webp` files. The `image` field is just the ID (no extension). Use `just blog::img` to convert and import from Downloads. Widescreen landscape format. `just blog::img` handles resizing and conversion.. The `image` field is optional — omit it if no image is available.

Guidelines:

**Tone & voice:**

- Default (Tako-kun): First person plural ("we") or neutral. Friendly, playful, catchy — like a mascot talking to a friend who codes.
- Dan Lilienblum posts: First person singular ("I"). More personal and opinionated.
- Light humor is welcome. Forced jokes are not.
- Never aggressive, never attack other products. When mentioning competitors, be genuinely respectful — they're building cool things too. Say what Tako does differently, not what others do wrong.
- Confident but not arrogant. "We chose X because Y" not "X is obviously better than Z."
- No corporate speak, no marketing buzzwords, no "leverage", no "empower", no "game-changer".
- No filler intros ("In today's fast-paced world..."). Get to the interesting part.

**Tako's vision (weave in where relevant):**

Tako isn't just a deploy tool — it's becoming the platform layer between your code and the internet. Today it handles deployment, routing, TLS, secrets, and local dev. The roadmap includes backend primitives like WebSocket/SSE channels, queues, workflows, and image optimization — things most apps bolt on as separate services. Most competitors (Kamal, Dokku, Coolify) stop at "get your code running." Tako wants to provide the infrastructure your app needs so you don't have to.

Combined with multi-server environments and Cloudflare Argo smart routing, Tako lets you build your own edge network on cheap VPS boxes worldwide — competitive with Fly.io, but on your own hardware.

**Structure & content:**

- **Length**: 400-800 words. Say what needs to be said, then stop.
- Short intro paragraph, 2-3 sections with h2 headings, brief closing.
- Every claim about Tako must be verifiable from SPEC.md or source code.
- Code examples when they clarify. Use real Tako commands/config, not pseudocode.
- **Backlinks are mandatory.** Every post must link to at least 2-3 relevant docs pages (e.g., `/docs`, `/docs/tako-toml`, `/docs/deployment`, `/docs/cli`, `/docs/development`). Link inline where concepts are mentioned — don't save all links for the end. Also link to the GitHub repo, other blog posts, or external resources where relevant. Think of each post as an entry point that guides readers deeper into Tako's docs.
- **Use tables for structured data.** When comparing tools, listing features, or presenting any data with multiple dimensions, use Markdown tables instead of prose or bullet lists. Tables are easier to scan and make comparisons obvious.
- **Use D2 diagrams for architecture and flows.** When explaining how components connect, data flows, or multi-step processes, use ` ```d2 ` code blocks. D2 renders to inline SVG at build time via `astro-d2` (sketch mode, Shirley Temple theme). Keep diagrams simple — they should clarify, not overwhelm. Good uses: deploy pipelines, request routing, component relationships. Bad uses: anything that's clearer as a sentence.

### Step 2b — Image prompt

Add an HTML comment right after the frontmatter with a ChatGPT image generation prompt. The user will paste it into ChatGPT, download the result, then run `just blog::img` to crop, convert to webp, and import it.

Format:

```markdown
---
layout: ../../layouts/BlogPostLayout.astro
title: "Post Title"
date: "YYYY-MM-DDTHH:MM"
description: "SEO description here"
image:
---

<!-- IMAGE PROMPT (copy-paste this entire block into ChatGPT):

Generate a wide illustration for a blog post hero image.

Character: A small, simple octopus. Reference: https://tako.sh/assets/logo.svg
The octopus must match the style of our logo — flat, minimal, no outlines, soft pastel coral pink body with simple dot eyes and a small curved mouth. Not 3D, not shiny, not glossy, not kawaii/cartoonish. Subtle and understated.

Scene: [Describe a specific scene or metaphor relevant to the blog post topic.]

Style requirements:
- Flat illustration with paper-like grain texture
- Light, airy, pastel tones — not saturated, not glossy, not 3D
- Color palette: coral pink (#E88783), mint teal (#9BC4B6), warm beige (#FFF9F4) background, dark purple (#2F2A44) accents
- Calm, warm, friendly mood — like a watercolor postcard
- Widescreen landscape format
- IMPORTANT COMPOSITION: All key objects and the main subject must be concentrated in a horizontal band in the CENTER of the image. Leave generous empty space (just background/sky/ground) at the TOP and BOTTOM edges. The image will be cropped to 5:2 ratio from the center — nothing important should be in the outer edges.

Output: a single image in widescreen landscape format.
-->
```

The prompt should be specific to the post's topic — not generic. Describe an actual scene or metaphor that fits the content. When describing mood or feeling, phrase it as a style direction, NOT as quoted text that ChatGPT might render literally.

After writing the post, copy the image prompt text (everything between the `<!-- IMAGE PROMPT` and `-->` markers, excluding the markers themselves) to the clipboard using `pbcopy`.

After the user downloads the image, they run `just blog::img` which crops to 5:2 from center, converts to webp, and outputs the hash ID to put in the `image:` frontmatter field.

### Step 3 — Verify

1. Run `just blog og` to generate/regenerate OG images (includes the new post).
2. Run `cd website && npx astro build` to confirm the post builds.
3. Check that the post appears in the blog listing page.
4. Show the user the post title, slug, and a brief summary for approval.

## Date

Always use today's date and current time (UTC) for the post. Format: `YYYY-MM-DDTHH:MM`. Get from the system.

## Slug

Derive from the title: lowercase, hyphens, no special characters. Keep it short.
Example: "Why We Don't Use Docker" → `why-we-dont-use-docker.md`

## Competitive Landscape

Tako exists in a crowded space. Keep these tools in mind when writing — reference them when relevant, position Tako honestly against them.

**CLI-based self-hosted (most similar to Tako):**

- **Kamal** (37signals) — Docker-based deploy via SSH, custom Go proxy (kamal-proxy). 13.9k stars. Ruby. The biggest name in this space thanks to DHH.
- **Sidekick** — Go CLI, turns VPS into mini-PaaS with Docker+Traefik. 7.4k stars. Markets as "your own Fly.io."
- **Piku** — Tiniest PaaS, git-push, no Docker, Python, runs on Raspberry Pi. 6.6k stars. Closest philosophy to Tako.
- **Exoframe** — One-command Docker deploys with Traefik. JS. 1.1k stars.

**Self-hosted PaaS (web UI, heavier):**

- **Coolify** — Open-source Heroku/Vercel alternative, full web UI. PHP. 51.8k stars. Dominant in this category.
- **Dokploy** — Lighter Coolify alternative, Docker Swarm. TS. 31.7k stars.
- **Dokku** — The OG mini-Heroku, git-push+Docker+buildpacks. 31.9k stars. Battle-tested.
- **CapRover** — Web dashboard PaaS, Docker Swarm. 14.9k stars.

**Cloud PaaS (hosted competitors):**

- **Fly.io** — Edge micro-VMs, CLI-driven. Popular indie dev choice.
- **Railway** — Great DX, auto-detect, built Nixpacks/Railpack.
- **Render** — Modern Heroku replacement.
- **SST** — TypeScript IaC on AWS. 25.7k stars.

**Reverse proxies (Tako uses Pingora):**

- **Caddy** — Auto HTTPS, Go. 70.9k stars. Used by Uncloud, Ptah.sh.
- **Traefik** — Cloud-native proxy, Docker/K8s auto-config. 62.2k stars. Used by Sidekick, Exoframe.
- **Pingora** — Cloudflare's Rust proxy framework. 26.3k stars. What Tako is built on.

**Dead projects (cautionary tales):**

- Nginx Unit — ARCHIVED Oct 2025
- HashiCorp Waypoint — ARCHIVED Jan 2024

**Tako's unique positioning:**

1. No Docker required (only Piku shares this)
2. Rust + Pingora proxy (no other deploy tool uses Pingora)
3. SFTP-based deployment (others use Docker registries or git push)
4. Native process management (processes, not containers)
5. Built-in local dev with HTTPS, DNS, and proxy
6. "Everything you need to run apps on your own hardware"

## Rules

- One post per invocation.
- Don't modify existing posts unless asked.
- Don't commit — let the user review first.
- Default author is "Tako-kun" (no frontmatter `author` field needed). To post under Dan's name, add `author: dan` to the frontmatter. Author lookup is in `BlogPostLayout.astro`. Only use `author: dan` if the user explicitly asks.
