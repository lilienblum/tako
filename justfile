mod run-debug 'just/run-debug.just'
mod release 'just/release.just'
mod test 'just/test.just'

# Top-level aliases.
tako *arguments: (run-debug::tako arguments)

clean: run-debug::clean

fmt:
    cargo fmt
    bun run fmt

build-tako-server: run-debug::build-tako-server

create-bun-server: run-debug::create-bun-server

install-bun-server: run-debug::install-bun-server

e2e fixture="e2e/js/tanstack-start": (test::e2e fixture)
