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
    out="$root/website/public/assets/blog/${id}.webp"
    mv "$tmp" "$out"
    echo "$id"
