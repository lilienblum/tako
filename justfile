mod run-debug 'just/run-debug.just'
mod release 'just/release.just'
mod test 'just/test.just'

# Backward-compatible aliases for existing top-level commands.
tako *arguments: (run-debug::tako arguments)

clean: run-debug::clean

build-tako-server: run-debug::build-tako-server

create-debug-server: run-debug::create-debug-server

install-server: run-debug::install-server
