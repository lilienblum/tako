set working-directory := ".."

# Build website and check for broken internal links
website:
    cd website && npx astro build --silent
    lychee --root-dir website/dist 'website/dist/**/*.html'
