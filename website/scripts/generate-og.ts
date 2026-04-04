import satori from "satori";
import { Resvg } from "@resvg/resvg-js";
import { readFileSync, writeFileSync, mkdirSync, existsSync } from "fs";
import { join } from "path";

const ROOT = join(import.meta.dir, "..");
const FONT_CACHE = join(ROOT, "node_modules/.cache/fonts");

async function fetchFont(family: string, weight: number): Promise<ArrayBuffer> {
  const safe = family.replace(/\s+/g, "_");
  const cached = join(FONT_CACHE, `${safe}-${weight}.ttf`);
  if (existsSync(cached)) return readFileSync(cached).buffer as ArrayBuffer;

  const css = await fetch(
    `https://fonts.googleapis.com/css2?family=${encodeURIComponent(family)}:wght@${weight}&display=swap`,
    {
      // Old UA → Google Fonts returns TTF (satori can't parse woff2)
      headers: { "User-Agent": "Mozilla/4.0" },
    },
  ).then((r) => r.text());

  const url = css.match(/src: url\(([^)]+)\)/)?.[1];
  if (!url) throw new Error(`No font URL found for ${family}:${weight}`);

  const data = await fetch(url).then((r) => r.arrayBuffer());
  mkdirSync(FONT_CACHE, { recursive: true });
  writeFileSync(cached, Buffer.from(data));
  return data;
}

function titleFontSize(title: string): number {
  if (title.length < 25) return 68;
  if (title.length < 40) return 58;
  if (title.length < 55) return 50;
  return 44;
}

export async function generateOgImage(title: string, outputPath: string) {
  const [poppinsBold, plexMono] = await Promise.all([
    fetchFont("Poppins", 700),
    fetchFont("IBM Plex Mono", 400),
  ]);

  const logoSvg = readFileSync(join(ROOT, "public/assets/logo.svg"), "utf-8");
  const logoUri = `data:image/svg+xml;base64,${Buffer.from(logoSvg).toString("base64")}`;

  const svg = await satori(
    {
      type: "div",
      props: {
        style: {
          width: "100%",
          height: "100%",
          display: "flex",
          flexDirection: "column",
          justifyContent: "space-between",
          padding: "60px 72px 52px",
          backgroundColor: "#FFF9F4",
          fontFamily: "Poppins",
        },
        children: [
          // coral accent bar at top
          {
            type: "div",
            props: {
              style: {
                position: "absolute",
                top: "0",
                left: "0",
                right: "0",
                height: "6px",
                background: "linear-gradient(90deg, #E88783 0%, #E88783 60%, #9BC4B6 100%)",
              },
            },
          },
          // title area
          {
            type: "div",
            props: {
              style: {
                display: "flex",
                flexDirection: "column",
                gap: "0",
                flexGrow: 1,
                justifyContent: "center",
              },
              children: [
                {
                  type: "div",
                  props: {
                    style: {
                      fontSize: `${titleFontSize(title)}px`,
                      fontWeight: 700,
                      color: "#2F2A44",
                      lineHeight: 1.2,
                      letterSpacing: "-0.02em",
                      maxWidth: "1000px",
                    },
                    children: title,
                  },
                },
              ],
            },
          },
          // bottom: logo + url
          {
            type: "div",
            props: {
              style: {
                display: "flex",
                alignItems: "center",
                gap: "14px",
              },
              children: [
                {
                  type: "img",
                  props: {
                    src: logoUri,
                    width: 48,
                    height: 48,
                  },
                },
                {
                  type: "div",
                  props: {
                    style: {
                      fontFamily: "IBM Plex Mono",
                      fontSize: "24px",
                      fontWeight: 400,
                      color: "#2F2A44",
                      letterSpacing: "0.01em",
                    },
                    children: "tako.sh/blog",
                  },
                },
              ],
            },
          },
        ],
      },
    },
    {
      width: 1200,
      height: 630,
      fonts: [
        { name: "Poppins", data: poppinsBold, weight: 700, style: "normal" },
        {
          name: "IBM Plex Mono",
          data: plexMono,
          weight: 400,
          style: "normal",
        },
      ],
    },
  );

  const resvg = new Resvg(svg, { fitTo: { mode: "width", value: 1200 } });
  const png = resvg.render().asPng();

  mkdirSync(join(outputPath, ".."), { recursive: true });
  writeFileSync(outputPath, png);
}

if (import.meta.main) {
  const [title, outputPath] = process.argv.slice(2);
  if (!title || !outputPath) {
    console.error("Usage: bun run generate-og.ts <title> <output-path>");
    process.exit(1);
  }
  await generateOgImage(title, outputPath);
  console.log(`og → ${outputPath.split("/").pop()}`);
}
