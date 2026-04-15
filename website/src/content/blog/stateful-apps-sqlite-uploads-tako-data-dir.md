---
title: "Stateful Apps on Tako: SQLite, Uploads, and the TAKO_DATA_DIR Contract"
date: "2026-04-14T10:00"
description: "TAKO_DATA_DIR is a persistent, per-app directory that survives deploys and rolling restarts — so SQLite, file uploads, and queue data just work on a single VPS without external storage."
image:
---

<!-- IMAGE PROMPT (copy-paste this entire block into ChatGPT):

Generate a wide illustration for a blog post hero image.

Character: A small, simple octopus. Reference: https://tako.sh/assets/logo.svg
The octopus must match the style of our logo — flat, minimal, no outlines, soft pastel coral pink body with simple dot eyes and a small curved mouth. Not 3D, not shiny, not glossy. Expressive and full of personality — eyes can squint, widen, or glance; the mouth can grin, gasp, or smirk; tentacles are always doing something. Stylized, not realistic, and not hyper-kawaii either.

Scene: The story is Geppetto in his workshop from Pinocchio — specifically the quiet, warm moment where the old craftsman sits surrounded by every toy and piece of work he has ever made, precisely arranged, lovingly tended, nothing lost. Our octopus is Geppetto.

(1) WHAT STORY: Geppetto in his workshop — the frame where the craftsman sits amid a lifetime of neatly organized creations, everything exactly where it belongs.
(2) WHICH MOMENT: The peaceful late-evening moment after a long day — tools put away, shelves orderly, a cup of something warm. The craftsman surveys their kingdom with quiet pride.
(3) What the octopus is doing: sitting at a wooden workbench, one tentacle resting on a small jar labeled "app.db", another pointing proudly at a shelf of neatly stacked folders labeled "uploads/". The octopus's expression is warm and satisfied — a slight smirk, eyes half-lidded with contentment.
(4) Emotional beat: serene ownership, quiet pride — "all of this is mine and none of it is going anywhere"
(5) Gesture: tentacles spread wide across the workspace in a "behold" gesture, one holding the SQLite jar up like a trophy, one resting on an open drawer of labeled folders. Through a small circular window in the background, you can see tiny cardboard shipping boxes (deploys) sailing past like ships — but they don't come inside. The workshop is untouched.

Style requirements:
- Flat illustration with paper-like grain texture
- Light, airy, pastel tones — not saturated, not glossy, not 3D
- Color palette: coral pink (#E88783), mint teal (#9BC4B6), warm beige (#FFF9F4) background, dark purple (#2F2A44) accents
- Playful, characterful, and full of motion — warm and friendly but lively. Think children's book spread or New Yorker cover, not corporate landing page. A soft sense of movement (flying confetti, dust puffs, motion lines, tilted angles) is welcome when it fits.
- Landscape orientation, roughly 16:9. The image will be displayed in a centered, boxed frame at about 640×360 — no cropping, the original aspect ratio is preserved, so compose the whole frame to be presentable.

Output: a single image in widescreen landscape format.
-->

The first thing most side projects outgrow isn't their server — it's the assumption that they don't need persistent storage.

You start with a stateless API. Static responses, external auth, everything in memory. Then someone asks for user preferences, or you want to store uploaded avatars, or you need to track some simple counts. And suddenly you're pricing out managed PostgreSQL.

On a $5 VPS, that's your entire hosting budget.

Tako ships `TAKO_DATA_DIR`: a persistent, per-app directory that outlives deploys and rolling restarts. SQLite, file uploads, queue data — anything that lives in a file works here, without an external service.

## The contract

`TAKO_DATA_DIR` is an environment variable pointing to a directory Tako owns and preserves. It's set automatically in both dev and production:

| Environment   | Path                                                  |
| ------------- | ----------------------------------------------------- |
| `tako dev`    | `.tako/data/app/` (inside your project)               |
| `tako deploy` | `/opt/tako/data/apps/{app}/data/app/` (on the server) |

You don't create this directory — Tako does. It persists across:

- **Deploys** — rolling restarts swap the release directory, not the data directory
- **Server restarts** and `tako-server` upgrades
- **Scale-to-zero** idle cycles — the directory is on disk, not in process memory

It's only cleaned up when you explicitly delete the app.

## SQLite without a managed database

SQLite is underrated for side projects. It's fast, reliable, needs zero infrastructure, and scales comfortably to millions of rows on any modern VPS. The only catch is that most deploy tools don't give you a reliable place to put the file.

`TAKO_DATA_DIR` is that place.

```typescript
import { Database } from "bun:sqlite";
import { Tako } from "tako.sh";
import { join } from "path";

const db = new Database(join(process.env.TAKO_DATA_DIR!, "app.db"));
db.run(`
  CREATE TABLE IF NOT EXISTS notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
  )
`);

export default Tako.serve({
  async fetch(req) {
    if (req.method === "POST" && new URL(req.url).pathname === "/notes") {
      const { body } = await req.json();
      db.run("INSERT INTO notes (body) VALUES (?)", [body]);
      return new Response("ok");
    }
    const notes = db.query("SELECT * FROM notes ORDER BY created_at DESC").all();
    return Response.json(notes);
  },
});
```

The database file lives at `$TAKO_DATA_DIR/app.db`. Deploy a new version and the release directory swaps, but `TAKO_DATA_DIR` stays put. Your rows are exactly where you left them.

## File uploads

The same pattern applies to any file-based storage:

```typescript
import { Tako } from "tako.sh";
import { writeFile, mkdir } from "fs/promises";
import { join } from "path";

const uploadsDir = join(process.env.TAKO_DATA_DIR!, "uploads");
await mkdir(uploadsDir, { recursive: true });

export default Tako.serve({
  async fetch(req) {
    if (req.method === "POST" && new URL(req.url).pathname === "/upload") {
      const formData = await req.formData();
      const file = formData.get("file") as File;
      await writeFile(join(uploadsDir, file.name), Buffer.from(await file.arrayBuffer()));
      return Response.json({ path: `/files/${file.name}` });
    }
    // serve files from uploadsDir...
  },
});
```

Uploaded files persist across deploys. New releases start, old ones drain — the files are untouched.

## Dev/prod parity

In development, `tako dev` sets `TAKO_DATA_DIR` to `.tako/data/app/` inside your project directory. Same env var, same code path, different location. No mocking, no special cases.

If you want a clean local state, delete `.tako/data/app/` — the same reasoning applies in production: the data persists until you intentionally clear it.

Run `tako typegen` and the generated `tako.d.ts` will include `TAKO_DATA_DIR` as a typed env var alongside your secrets, so your editor knows it's always available.

## Where this doesn't replace managed infrastructure

`TAKO_DATA_DIR` is a single-server guarantee. If you're running the same app across multiple servers, each server has its own independent data directory — they don't sync. For multi-server setups you'll want either:

- An external database (Turso, PlanetScale, Neon)
- SQLite replication (LiteFS or Litestream) pointed at `TAKO_DATA_DIR`
- Architecture that avoids shared mutable state

For most side projects on a single server, none of that is necessary. A SQLite file in `TAKO_DATA_DIR` handles the load, survives the deploys, and costs nothing extra.

## Try it

`TAKO_DATA_DIR` is set automatically on every deploy — no configuration required.

```bash
tako deploy
# on the server, your app sees:
# TAKO_DATA_DIR=/opt/tako/data/apps/{app}/data/app
```

See the [deployment docs](/docs/deployment) for the full setup, the [development guide](/docs/development) for how data directories behave locally, and the [CLI reference](/docs/cli) for app lifecycle commands including `tako app delete`.
