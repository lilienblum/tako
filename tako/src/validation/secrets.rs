use crate::config::SecretsStore;

use super::ValidationResult;

/// Validate secrets configuration
pub fn validate_secrets(secrets: &SecretsStore) -> ValidationResult {
    let mut result = ValidationResult::new();

    // Check for discrepancies (secrets missing in some environments)
    let discrepancies = secrets.find_discrepancies();
    for discrepancy in &discrepancies {
        result.warn(format!(
            "Secret '{}' is missing in environments: {}",
            discrepancy.name,
            discrepancy.missing_in.join(", ")
        ));
    }

    result
}

/// Validate secrets for a specific environment
///
/// This is stricter than general validation - any missing secrets
/// compared to other environments is an error, not a warning.
pub fn validate_secrets_for_env(secrets: &SecretsStore, env_name: &str) -> ValidationResult {
    let mut result = ValidationResult::new();

    let discrepancies = secrets.find_discrepancies();
    for discrepancy in &discrepancies {
        if discrepancy.missing_in.contains(&env_name.to_string()) {
            result.error(format!(
                "Secret '{}' is missing for environment '{}'. \
                 Run 'tako secret set --env {} {}' to set it.",
                discrepancy.name, env_name, env_name, discrepancy.name
            ));
        }
    }

    result
}

/// Pre-deployment validation of secrets
///
/// Ensures all secrets are complete for the target environment.
/// Returns errors if any secrets are missing.
pub fn validate_secrets_for_deployment(secrets: &SecretsStore, env_name: &str) -> ValidationResult {
    let mut result = ValidationResult::new();

    // First check if environment has any secrets at all
    let env_secrets = secrets.get_env(env_name);
    if env_secrets.is_none() || env_secrets.map(|s| s.is_empty()).unwrap_or(true) {
        // If no environments have secrets, this is fine
        if secrets.is_empty() {
            return result;
        }

        // But if other environments have secrets, this environment should too
        result.error(format!(
            "Environment '{}' has no secrets configured, but other environments do. \
             Run 'tako secret sync' to sync secrets.",
            env_name
        ));
        return result;
    }

    // Check for missing secrets compared to other environments
    result.merge(validate_secrets_for_env(secrets, env_name));

    result
}
