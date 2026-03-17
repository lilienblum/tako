#!/usr/bin/env bash
set -euo pipefail

# Generate a blog hero image from a markdown file's IMAGE PROMPT comment.
# Usage: blog-img-generate.sh <blog-post.md>
# Requires: FAL_KEY env var or fish universal variable

post="$1"

if [ -z "$post" ]; then
  echo "Usage: blog-img-generate.sh <path-to-blog-post.md>" >&2
  exit 1
fi

# If FAL_KEY not in env, try reading from fish universal variables
if [ -z "${FAL_KEY:-}" ]; then
  FAL_KEY=$(fish -c 'echo $FAL_KEY' 2>/dev/null)
fi

if [ -z "${FAL_KEY:-}" ]; then
  echo "FAL_KEY not found. Set it with: set -Ux FAL_KEY your-key" >&2
  exit 1
fi

# Extract prompt from HTML comment
prompt=$(sed -n '/<!-- IMAGE PROMPT/,/-->/p' "$post" | sed '1d;$d')

if [ -z "$prompt" ]; then
  echo "No IMAGE PROMPT comment found in $post" >&2
  exit 1
fi

echo "Generating image via fal.ai..."

response=$(curl -s -X POST "https://fal.run/fal-ai/flux/dev" \
  -H "Authorization: Key ${FAL_KEY}" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg prompt "$prompt" '{
    prompt: $prompt,
    image_size: { width: 1400, height: 560 },
    num_inference_steps: 28,
    guidance_scale: 3.5,
    num_images: 1,
    output_format: "png"
  }')")

image_url=$(echo "$response" | jq -r '.images[0].url // empty')

if [ -z "$image_url" ]; then
  echo "Failed to generate image. Response:" >&2
  echo "$response" >&2
  exit 1
fi

# Download
tmp_png=$(mktemp /tmp/blog-img-XXXXXX.png)
curl -s -o "$tmp_png" "$image_url"

# Convert to webp
mkdir -p website/public/assets/blog
tmp_webp=$(mktemp /tmp/blog-img-XXXXXX.webp)
cwebp -q 85 "$tmp_png" -o "$tmp_webp" >/dev/null 2>&1
rm "$tmp_png"

# Name by content hash
id=$(shasum -a 256 "$tmp_webp" | cut -c1-12)
out="website/public/assets/blog/${id}.webp"
mv "$tmp_webp" "$out"

# Update frontmatter image field
if grep -q '^image:' "$post"; then
  sed -i '' "s/^image:.*/image: ${id}/" "$post"
else
  sed -i '' "/^date:/a\\
image: ${id}
" "$post"
fi

# Remove the IMAGE PROMPT comment
sed -i '' '/<!-- IMAGE PROMPT/,/-->/d' "$post"

echo "Done: $id"
echo "Saved: $out"
