# Regenerate OG images for all blog posts
og:
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{ source_directory() }}/.."
    blog_dir="$root/website/src/content/blog"
    mkdir -p "$root/website/public/assets/blog/og"
    for post in "$blog_dir"/*.md; do
        title=$(sed -n 's/^title: *"\{0,1\}\([^"]*\)"\{0,1\} *$/\1/p' "$post" | head -1)
        slug=$(basename "$post" .md)
        if [ -n "$title" ]; then
            (cd "$root/website" && bun run scripts/generate-og.tsx "$title" "public/assets/blog/og/${slug}.png")
        fi
    done

# Import a local image as blog hero (from ~/Downloads, or specify filename)
img file="":
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{ source_directory() }}/.."
    if [ -z "{{ file }}" ]; then
        src=$(ls -t "$HOME/Downloads"/*.png 2>/dev/null | head -1)
        if [ -z "$src" ]; then
            echo "No .png found in ~/Downloads" >&2
            exit 1
        fi
    else
        src="$HOME/Downloads/{{ file }}"
    fi
    mkdir -p "$root/website/public/assets/blog"
    max_w=1400
    tmp_src=$(mktemp /tmp/blog-img-XXXXXX).png
    cp "$src" "$tmp_src"
    # Resize width to max if needed
    cur_w=$(sips -g pixelWidth "$tmp_src" | tail -1 | awk '{print $2}')
    if [ "$cur_w" -gt "$max_w" ]; then
        sips --resampleWidth "$max_w" "$tmp_src" --out "$tmp_src" >/dev/null 2>&1
    fi
    # Center-crop to 5:2 ratio
    cur_w=$(sips -g pixelWidth "$tmp_src" | tail -1 | awk '{print $2}')
    cur_h=$(sips -g pixelHeight "$tmp_src" | tail -1 | awk '{print $2}')
    target_h=$(( cur_w * 2 / 5 ))
    if [ "$cur_h" -gt "$target_h" ]; then
        crop_y=$(( (cur_h - target_h) / 2 ))
        sips --cropOffset "$crop_y" 0 --cropToHeightWidth "$target_h" "$cur_w" "$tmp_src" --out "$tmp_src" >/dev/null 2>&1
    fi
    # Convert to webp
    tmp=$(mktemp /tmp/blog-img-XXXXXX).webp
    cwebp -q 85 "$tmp_src" -o "$tmp" >/dev/null 2>&1
    rm "$tmp_src"
    id=$(shasum -a 256 "$tmp" | cut -c1-12)
    out="$root/website/public/assets/blog/${id}.webp"
    mv "$tmp" "$out"
    rm "$src"
    # Find the most recent blog post and patch it
    blog_dir="$root/website/src/content/blog"
    post=$(ls -t "$blog_dir"/*.md 2>/dev/null | head -1)
    if [ -n "$post" ]; then
        # Insert hash into image: frontmatter
        sed -i '' 's/^image:.*$/image: '"$id"'/' "$post"
        # Remove IMAGE PROMPT comment block
        sed -i '' '/^<!-- IMAGE PROMPT/,/^-->/d' "$post"
        echo "$id → $(basename "$post")"
    else
        echo -n "$id" | pbcopy
        echo "$id (copied)"
    fi
