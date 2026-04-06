set working-directory := ".."

# Build website and check for broken internal links
website:
    cd website && npx astro build --silent
    find website/dist -name '*.html' ! -name 'index.html' | while read -r file; do \
        clean="${file%.html}"; \
        mkdir -p "$clean"; \
        cp "$file" "$clean/index.html"; \
    done
    lychee --root-dir website/dist --exclude 'dub\.sh' --exclude 'pkg\.go\.dev' --exclude 'tako\.sh/blog' --exclude 'npmjs\.com/package/tako\.sh' 'website/dist/**/*.html'
