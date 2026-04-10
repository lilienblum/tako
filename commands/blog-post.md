---
description: Create a new blog post from a topic or idea
---

$ARGUMENTS

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
2. Read existing blog posts in `website/src/content/blog/` to match tone and style.
3. Check memory for competitor landscape data (reference_competitor_landscape.md) for context on similar tools.
4. If the topic involves a specific feature, read the relevant source code to get details right.
5. If the topic involves competitors or external tools, do web research to get current facts (stars, versions, status).
6. **Fact-check rigorously.** Every factual claim (star counts, release dates, feature support, version numbers) must be verified against at least two independent sources. Cross-reference docs, GitHub, and web search results. If two sources disagree, dig until you find the truth. Do not publish a number you only saw once.

### Step 2 — Write

Create a new markdown file at `website/src/content/blog/{slug}.md` with this frontmatter:

```markdown
---
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
title: "Post Title"
date: "YYYY-MM-DDTHH:MM"
description: "SEO description here"
image:
---

<!-- IMAGE PROMPT (copy-paste this entire block into ChatGPT):

Generate a wide illustration for a blog post hero image.

Character: A small, simple octopus. Reference: https://tako.sh/assets/logo.svg
The octopus must match the style of our logo — flat, minimal, no outlines, soft pastel coral pink body with simple dot eyes and a small curved mouth. Not 3D, not shiny, not glossy. Expressive and full of personality — eyes can squint, widen, or glance; the mouth can grin, gasp, or smirk; tentacles are always doing something. Stylized, not realistic, and not hyper-kawaii either.

Scene: [This is the most important section. Do not describe a "scene" — describe a STORY MOMENT.

The difference: a scene is decorative ("octopus holding three servers"). A story moment is a single frame from a larger narrative the viewer already half-knows ("the moment the musketeers cross blades above the crown," "the moment Luke looks up at the twin suns," "the moment the heist crew leans over the blueprint"). A story moment arrives pre-loaded with tone, stakes, and meaning because the viewer's brain fills in the rest from memory.

Anchor the image in a recognizable story, film, painting, myth, or genre trope that actually matches the post's content and mood (see the story menu below). Then pin down:
  (1) WHAT STORY is this a moment from? (e.g. "The Three Musketeers crossing blades," "Hokusai's Great Wave," "the Ocean's Eleven planning table")
  (2) WHICH moment inside that story? (the triumphant one, the quiet-before-the-storm one, the "oh no" one)
  (3) What verb is the octopus (or octopuses) physically doing in that moment?
  (4) What emotional beat does each octopus carry? (determined, delighted, mischievous, proud, nervous, triumphant)
  (5) What gesture makes each beat legible? (tentacles raised, pointing, bracing, mid-throw, arms crossed)

Aim for "single frame from a story I recognize," not "mascot standing in a scene." Whimsical props and slightly absurd juxtapositions are encouraged when they fit the story you're borrowing from.]

Style requirements:
- Flat illustration with paper-like grain texture
- Light, airy, pastel tones — not saturated, not glossy, not 3D
- Color palette: coral pink (#E88783), mint teal (#9BC4B6), warm beige (#FFF9F4) background, dark purple (#2F2A44) accents
- Playful, characterful, and full of motion — warm and friendly but lively. Think children's book spread or New Yorker cover, not corporate landing page. A soft sense of movement (flying confetti, dust puffs, motion lines, tilted angles) is welcome when it fits.
- Widescreen landscape format
- IMPORTANT COMPOSITION: All key objects and the main subject must be concentrated in a horizontal band in the CENTER of the image. Leave generous empty space (just background/sky/ground) at the TOP and BOTTOM edges. The image will be cropped to 5:2 ratio from the center — nothing important should be in the outer edges.

Output: a single image in widescreen landscape format.
-->
```

The prompt should be specific to the post's topic — not generic. The #1 failure mode is a tidy but lifeless "object + object" composition where the octopus just stands next to some props. The fix is not more motion lines or a better pose — the fix is **telling a story**. A hero image works when it borrows a single frame from a story the viewer already knows (a film, a painting, a myth, a genre trope) and the image arrives pre-loaded with tone, stakes, and meaning. Without a story anchor you're just decorating; with one, you're telling.

**Pick the story before you write the prompt.** Start by asking: _"If this blog post were a movie or a famous painting, which one would it be?"_ Then take one specific frame from that and cast the octopus into it. The reference should actually match the post's content and mood — don't force it, and don't default to the same reference across adjacent posts. Variety matters; context matters more.

**These next examples are just that — examples.** They're here to unblock you when you're staring at a blank prompt, not to limit you. The right story for a given post is almost never going to be on any pre-made list; it's whatever actually matches the specific angle, tone, and details of _that_ post. Treat the table as a sampler of the _kinds_ of places to look (films, paintings, myths, genre tropes, famous scenes), then go find your own. If none of these feel right, that's expected — invent something.

| Post vibe                              | A few example stories in that territory                                                                                    |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| Distributed / coordinated / consensus  | The Three Musketeers ("all for one"), Ocean's Eleven planning table, Fellowship of the Ring setting out, Avengers assemble |
| Scale / resilience / weathering load   | Hokusai's Great Wave, Atlas holding the world, lighthouse keepers in a storm, Moby Dick                                    |
| Speed / performance / racing           | Mad Max: Fury Road, Wacky Races, Formula 1 pit stop, the Kessel Run                                                        |
| Local dev / solo craft / quiet focus   | Studio Ghibli workshop scenes, Hopper's _Nighthawks_, Gepetto in his workshop, _Ratatouille_ kitchen                       |
| Migration / escape from a heavier tool | _The Great Escape_, Exodus, _Shawshank Redemption_ crawling to freedom, Hobbit leaving the Shire                           |
| Construction / building a system up    | Tower of Babel, medieval cathedral build, ant colony at work, Lego master-builders                                         |
| Security / secrets / protection        | Indiana Jones temple run, Smaug on his hoard, Gringotts vault, classic safecracker noir                                    |
| Orchestration / many things in harmony | Symphony conductor, ballet corps, Rube Goldberg machines, _Fantasia_'s Sorcerer's Apprentice                               |
| Launch / announcement / triumph        | Apollo liftoff, Olympic podium, _Rocky_ on the steps, flag-planting on a mountaintop                                       |
| Debugging / detective work             | Sherlock Holmes at the crime scene, film noir detective, _Columbo_, _Knives Out_ drawing room                              |

Again: the table is a nudge, not a box. A post about WebSocket channels might borrow from a pneumatic-tube mailroom. A post about cold starts might borrow from a sleeping dragon waking up. A post about secrets might borrow from a heist, a spy film, or a children's diary-with-a-lock. Pick whatever actually tells the right story for _this_ post, and don't feel obliged to reach for anything on the list above.

**Quick self-check before finalizing the prompt:**

- **What story is this a frame from?** (If you can't name it in one sentence, you don't have one yet.)
- **What moment inside that story?** (The setup? The triumph? The "oh no" beat?)
- **Is there a verb?** ("standing," "holding," "next to" don't count.)
- **Is there a facial expression per octopus, and do they differ?**
- **Are the tentacles doing something specific, or just hanging?**
- **Would a developer glancing at it for half a second think "oh, that's [reference]!" — and smile?**

If the answer to "what story?" is fuzzy, stop and pick one before writing the scene. Everything else flows from that.

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
