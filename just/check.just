set working-directory := ".."

# Build website and check for broken internal links
website:
    bun run --filter website build
    lychee \
        --root-dir website/dist \
        --exclude 'pkg\.go\.dev' \
        --exclude 'npmjs\.com/package/tako\.sh' \
        --exclude 'x\.com/dhh' \
        'website/dist/**/*.html'
