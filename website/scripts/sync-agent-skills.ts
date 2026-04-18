#!/usr/bin/env bun
// Syncs agent skills from sdk/javascript/skills/ into public/.well-known/agent-skills/
// and writes an index.json per the Agent Skills Discovery RFC v0.2.0.
//
// Runs before astro build so the generated files are picked up as static assets.

import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(fileURLToPath(new URL(".", import.meta.url)), "..");
const REPO_ROOT = path.resolve(ROOT, "..");
const SRC_SKILLS = path.join(REPO_ROOT, "sdk/javascript/skills");
const DEST_DIR = path.join(ROOT, "public/.well-known/agent-skills");
const SITE = "https://tako.sh";

function parseFrontmatter(md: string): Record<string, string> {
  const match = md.match(/^---\n([\s\S]*?)\n---/);
  if (!match) return {};
  const out: Record<string, string> = {};
  let key: string | null = null;
  let buf: string[] = [];
  for (const line of match[1]!.split("\n")) {
    const m = line.match(/^([a-zA-Z0-9_-]+):\s*(.*)$/);
    if (m) {
      if (key) out[key] = buf.join(" ").trim();
      key = m[1]!;
      buf = m[2] ? [m[2]!] : [];
    } else if (key && line.trim()) {
      buf.push(line.trim());
    }
  }
  if (key) out[key] = buf.join(" ").trim();
  for (const k of Object.keys(out)) {
    out[k] = out[k]!.replace(/^>-\s*/, "").replace(/^["']|["']$/g, "");
  }
  return out;
}

async function main() {
  const entries = await readdir(SRC_SKILLS, { withFileTypes: true });
  const skills: Array<{
    name: string;
    type: string;
    description: string;
    url: string;
    sha256: string;
  }> = [];

  await mkdir(DEST_DIR, { recursive: true });

  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const slug = entry.name;
    const srcPath = path.join(SRC_SKILLS, slug, "SKILL.md");
    const content = await readFile(srcPath, "utf8");
    const frontmatter = parseFrontmatter(content);
    const destDir = path.join(DEST_DIR, slug);
    await mkdir(destDir, { recursive: true });
    await writeFile(path.join(destDir, "SKILL.md"), content);
    const sha256 = createHash("sha256").update(content).digest("hex");
    skills.push({
      name: slug,
      type: frontmatter["type"] ?? "framework",
      description: frontmatter["description"] ?? "",
      url: `${SITE}/.well-known/agent-skills/${slug}/SKILL.md`,
      sha256,
    });
  }
  skills.sort((a, b) => a.name.localeCompare(b.name));

  const index = {
    $schema: "https://agentskills.io/schemas/v0.2.0/index.json",
    skills,
  };
  await writeFile(path.join(DEST_DIR, "index.json"), JSON.stringify(index, null, 2) + "\n");
  console.log(`sync-agent-skills: wrote ${skills.length} skill(s) + index.json`);
}

await main();
