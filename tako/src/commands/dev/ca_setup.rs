//! Local CA Setup
//!
//! Handles the one-time setup of the Tako local CA for development HTTPS.

use crate::dev::{LocalCA, LocalCAStore};
use crate::output;

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
    "Trust the Tako local CA for trusted https://*.tako.test"
}

fn plan_ca_setup(ca_exists: bool, ca_trusted: bool) -> CaSetupPlan {
    CaSetupPlan {
        source: if ca_exists {
            CaSource::Existing
        } else {
            CaSource::Generated
        },
        // Default flow: ensure trusted HTTPS without prompting.
        install_trust: !ca_trusted,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn pending_sudo_action() -> Result<Option<&'static str>, Box<dyn std::error::Error>> {
    let store = LocalCAStore::new()?;
    let plan = plan_ca_setup(store.ca_exists(), store.is_ca_trusted());
    Ok(plan.install_trust.then_some(sudo_action_line()))
}

/// Setup the local CA for development
///
/// This will:
/// 1. Check if a CA already exists in the keychain
/// 2. If not, generate a new Root CA
/// 3. Check if the CA is trusted in the system trust store
/// 4. If not, install it in the system trust store (requires sudo)
pub async fn setup_local_ca() -> Result<LocalCA, Box<dyn std::error::Error>> {
    let store = LocalCAStore::new()?;

    let ca_exists = store.ca_exists();
    let ca_trusted = store.is_ca_trusted();
    let plan = plan_ca_setup(ca_exists, ca_trusted);

    if plan.install_trust && !output::is_interactive() {
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
        assert!(line.contains("https://*.tako.test"));
    }
}
