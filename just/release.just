set working-directory := ".."

# Publish all Rust crates in dependency order.
cargo:
    cargo publish -p tako-core
    cargo publish -p tako-socket
    cargo publish -p tako-server
    cargo publish -p tako

cargo-one package:
    cargo publish -p {{package}}

npm:
    cd sdk && npm publish

all: cargo npm
