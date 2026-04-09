//! Local CA setup and TLS certificate material for development HTTPS.

use std::path::{Path, PathBuf};

use sha2::Digest;

use crate::dev::{LocalCA, LocalCAStore};
use crate::output;

// ── CA setup ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaSource {
    Existing,
    Generated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CaSetupPlan {
    source: CaSource,
    install_trust: bool,
}

fn sudo_action_line() -> &'static str {
    "Trust the Tako local CA for trusted https://*.test"
}

fn plan_ca_setup(ca_exists: bool, ca_trusted: bool) -> CaSetupPlan {
    CaSetupPlan {
        source: if ca_exists {
            CaSource::Existing
        } else {
            CaSource::Generated
        },
        install_trust: !ca_trusted,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn pending_sudo_action() -> Result<Option<&'static str>, Box<dyn std::error::Error>> {
    let store = LocalCAStore::new()?;
    let plan = plan_ca_setup(store.ca_exists(), store.is_ca_trusted());
    Ok(plan.install_trust.then_some(sudo_action_line()))
}

/// Setup the local CA for development.
///
/// 1. Load existing or generate new Root CA
/// 2. Install trust in the system store if needed (requires sudo)
pub async fn setup_local_ca() -> Result<LocalCA, Box<dyn std::error::Error>> {
    let store = LocalCAStore::new()?;

    let ca_exists = store.ca_exists();
    let ca_trusted = store.is_ca_trusted();
    let plan = plan_ca_setup(ca_exists, ca_trusted);

    if plan.install_trust && !output::is_interactive() && !output::is_root() {
        return Err(
            "local CA is not trusted; run `tako dev` interactively once to install it".into(),
        );
    }

    let ca = match plan.source {
        CaSource::Existing => {
            tracing::debug!("Loading existing Tako CA from store…");
            let _t = output::timed("Load existing CA");
            store.load_ca()?
        }
        CaSource::Generated => {
            tracing::debug!("No existing CA found, generating new Tako CA…");
            let ca = {
                let _t = output::timed("Generate CA");
                output::with_spinner(
                    "Generating new Tako CA",
                    "Tako CA generated",
                    LocalCA::generate,
                )
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?
            };
            tracing::debug!("Saving generated CA to secure storage…");
            {
                let _t = output::timed("Save CA to store");
                output::with_spinner("Saving Tako CA to secure storage", "Tako CA saved", || {
                    store.save_ca(&ca)
                })
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
            }
            ca
        }
    };

    if plan.install_trust {
        tracing::debug!("CA not yet trusted in system store, installing trust…");
        output::info("Installing Tako CA in system trust store (sudo)...");
        let _t = output::timed("Install CA trust");
        store
            .install_ca_trust()
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        output::success("Tako CA trusted by system.");
    }

    Ok(ca)
}

// ── Leaf certificate material ────────────────────────────────────────────────

const DEV_TLS_CERT_FILENAME: &str = "fullchain.pem";
const DEV_TLS_KEY_FILENAME: &str = "privkey.pem";
const DEV_TLS_NAMES_FILENAME: &str = "names.json";
const DEV_TLS_CA_FINGERPRINT_FILENAME: &str = "ca_fingerprint";

pub(crate) fn dev_server_tls_paths_for_home(home: &Path) -> (PathBuf, PathBuf) {
    let certs_dir = home.join("certs");
    (
        certs_dir.join(DEV_TLS_CERT_FILENAME),
        certs_dir.join(DEV_TLS_KEY_FILENAME),
    )
}

pub(crate) fn dev_server_tls_names_path_for_home(home: &Path) -> PathBuf {
    home.join("certs").join(DEV_TLS_NAMES_FILENAME)
}

fn default_dev_tls_names_for_app(app_name: &str) -> Vec<String> {
    let d = crate::dev::TAKO_DEV_DOMAIN;
    let s = crate::dev::SHORT_DEV_DOMAIN;
    vec![
        format!("*.{s}"),
        s.to_string(),
        format!("{app_name}.{s}"),
        format!("*.{app_name}.{s}"),
        format!("*.{d}"),
        d.to_string(),
        format!("{app_name}.{d}"),
        format!("*.{app_name}.{d}"),
    ]
}

fn normalize_tls_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

fn load_dev_tls_names(path: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<Vec<String>>(&raw).ok()?;
    Some(normalize_tls_names(parsed))
}

pub(crate) fn ca_fingerprint(ca: &LocalCA) -> String {
    hex::encode(sha2::Sha256::digest(ca.ca_cert_pem().as_bytes()))
}

pub(crate) fn ca_fingerprint_path_for_home(home: &Path) -> PathBuf {
    home.join("certs").join(DEV_TLS_CA_FINGERPRINT_FILENAME)
}

fn ca_fingerprint_matches(ca: &LocalCA, home: &Path) -> bool {
    let fp_path = ca_fingerprint_path_for_home(home);
    let Ok(stored) = std::fs::read_to_string(&fp_path) else {
        return false;
    };
    stored.trim() == ca_fingerprint(ca)
}

pub(crate) fn ensure_dev_server_tls_material_for_home(
    ca: &LocalCA,
    home: &Path,
    app_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let (cert_path, key_path) = dev_server_tls_paths_for_home(home);
    let names_path = dev_server_tls_names_path_for_home(home);
    let have_cert_material = cert_path.is_file() && key_path.is_file();
    let ca_matches = ca_fingerprint_matches(ca, home);
    let existing_names = if have_cert_material {
        load_dev_tls_names(&names_path)
    } else {
        None
    };
    let mut names = default_dev_tls_names_for_app(app_name);
    if let Some(existing) = existing_names.clone() {
        names.extend(existing);
    }
    let names = normalize_tls_names(names);

    if have_cert_material
        && ca_matches
        && existing_names
            .as_ref()
            .is_some_and(|existing| *existing == names)
    {
        return Ok(false);
    }

    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let name_refs: Vec<&str> = names.iter().map(|name| name.as_str()).collect();
    let cert = ca.generate_leaf_cert_for_names(&name_refs)?;
    std::fs::write(&cert_path, cert.cert_pem.as_bytes())?;
    std::fs::write(&key_path, cert.key_pem.as_bytes())?;
    std::fs::write(&names_path, serde_json::to_string_pretty(&names)?)?;
    std::fs::write(ca_fingerprint_path_for_home(home), ca_fingerprint(ca))?;
    Ok(true)
}

pub(crate) fn ensure_dev_server_tls_material(
    ca: &LocalCA,
    app_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let data_dir = crate::paths::tako_data_dir()?;
    ensure_dev_server_tls_material_for_home(ca, &data_dir, app_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_existing_and_trusted_does_nothing() {
        let plan = plan_ca_setup(true, true);
        assert_eq!(
            plan,
            CaSetupPlan {
                source: CaSource::Existing,
                install_trust: false
            }
        );
    }

    #[test]
    fn plan_existing_but_untrusted_installs_trust_without_prompting() {
        let plan = plan_ca_setup(true, false);
        assert_eq!(
            plan,
            CaSetupPlan {
                source: CaSource::Existing,
                install_trust: true
            }
        );
    }

    #[test]
    fn plan_missing_ca_generates_and_installs_trust() {
        let plan = plan_ca_setup(false, false);
        assert_eq!(
            plan,
            CaSetupPlan {
                source: CaSource::Generated,
                install_trust: true
            }
        );
    }

    #[test]
    fn sudo_action_line_mentions_trusted_local_domains() {
        let line = sudo_action_line();
        assert!(line.contains("local CA"));
        assert!(line.contains("https://*.test"));
    }
}
