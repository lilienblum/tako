mod build 'just/build.just'
mod testbed 'just/testbed.just'
mod release 'just/release.just'
mod test 'just/test.just'

export TAKO_HOME := "local-dev/.tako"

tako *arguments:
    cargo build -p tako --release
    TAKO_HOME="$(pwd)/{{ TAKO_HOME }}" ./target/release/tako {{ arguments }}

fmt:
    cargo fmt
    bun run fmt

lint:
    cargo clippy --fix --allow-dirty --workspace --all-targets
    bun run lint
    bun run --filter '*' typecheck

ci: fmt lint test::all

# Build website and check for broken internal links
links:
    cd website && npx astro build --silent
    lychee --root-dir website/dist 'website/dist/**/*.html'

e2e fixture="e2e/fixtures/js/tanstack-start": (test::e2e fixture)

blog-img-gen post:
    ./scripts/blog-img-generate.sh {{ post }}

blog-img file="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "{{ file }}" ]; then
        src=$(ls -t "$HOME/Downloads"/ChatGPT\ Image*.png 2>/dev/null | head -1)
        if [ -z "$src" ]; then
            echo "No ChatGPT Image found in ~/Downloads" >&2
            exit 1
        fi
    else
        src="$HOME/Downloads/{{ file }}"
    fi
    mkdir -p website/public/assets/blog
    max_w=1400
    tmp_src=$(mktemp /tmp/blog-img-XXXXXX.png)
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
    tmp=$(mktemp /tmp/blog-img-XXXXXX.webp)
    cwebp -q 85 "$tmp_src" -o "$tmp" >/dev/null 2>&1
    rm "$tmp_src"
    id=$(shasum -a 256 "$tmp" | cut -c1-12)
    out="website/public/assets/blog/${id}.webp"
    mv "$tmp" "$out"
    echo "$id"
