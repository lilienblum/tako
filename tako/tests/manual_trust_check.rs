// Manual diagnostic — run with `cargo test --test manual_trust_check --
// --ignored --nocapture` to check the CA-trust state on the current
// machine without touching it. Never runs in CI.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual — reads the real user's Tako CA"]
fn check_real_trust_state() {
    use tako::dev::LocalCAStore;
    let store = LocalCAStore::new().unwrap();
    println!("ca_exists: {}", store.ca_exists());
    println!("is_ca_trusted: {}", store.is_ca_trusted());
}
