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

fn sudo_trust_explanation_lines() -> [&'static str; 3] {
    [
        "One-time sudo required to trust the Tako local CA.",
        "This enables trusted https://*.tako.local without browser warnings.",
        "Tako only updates the system trust entry and then continues startup.",
    ]
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

    let ca = match plan.source {
        CaSource::Existing => store.load_ca()?,
        CaSource::Generated => {
            let ca = output::with_spinner("Generating new Tako CA...", LocalCA::generate)?
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
            output::with_spinner("Saving Tako CA to secure storage...", || store.save_ca(&ca))?
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
            output::success("Tako CA generated and saved.");
            ca
        }
    };

    if plan.install_trust {
        // Keep this non-spinner so the sudo password prompt is obvious.
        output::warning("Sudo password required.");
        for line in sudo_trust_explanation_lines() {
            output::muted(line);
        }
        output::muted("Enter your password at the prompt below.");
        output::step("Installing Tako CA in system trust store (sudo)...");
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
    fn sudo_trust_explanation_mentions_trusted_local_domains() {
        let lines = sudo_trust_explanation_lines();
        assert!(lines[0].contains("sudo"));
        assert!(lines[1].contains("https://*.tako.local"));
    }
}
