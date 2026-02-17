use std::env::current_dir;
use std::fs;

use crate::app::resolve_app_name;
use crate::output;
use crate::runtime::detect_runtime;

pub fn run(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;
    let tako_toml_path = project_dir.join("tako.toml");

    // Check if tako.toml already exists
    if tako_toml_path.exists() && !force {
        if output::is_interactive()
            && output::confirm("tako.toml already exists. Overwrite it?", false)?
        {
            output::warning("Overwriting existing tako.toml");
        } else {
            return Err("tako.toml already exists. Use --force to overwrite.".into());
        }
    }

    // Detect runtime
    let runtime = detect_runtime(&project_dir);
    let runtime_name = runtime.as_ref().map(|r| r.name()).unwrap_or("unknown");

    // Resolve app name
    let app_name = resolve_app_name(&project_dir).unwrap_or_else(|_| "my-app".to_string());

    // Generate tako.toml
    let template = generate_template(&app_name, runtime_name);

    fs::write(&tako_toml_path, template)?;

    output::success("Created tako.toml");

    output::section("Detected");
    output::step(&format!("Runtime: {}", runtime_name));
    output::step(&format!("App name: {}", app_name));

    output::section("Next Steps");
    output::step("1. Edit tako.toml to configure environments and routes");
    output::step(&format!(
        "2. Run {} to add deployment servers",
        output::emphasized("tako servers add <host>")
    ));
    output::step(&format!(
        "3. Run {} to add secrets",
        output::emphasized("tako secrets set --env production <NAME>")
    ));
    output::step(&format!(
        "4. Run {} to deploy your app",
        output::emphasized("tako deploy")
    ));

    Ok(())
}

fn generate_template(app_name: &str, _runtime: &str) -> String {
    format!(
        r#"# Tako configuration
# tako.toml reference: https://tako.sh/docs/tako-toml

# Stable app identifier used for deploy paths and local dev hostnames.
# Set once and do not change after first deploy.
# If omitted, Tako auto-detects from runtime metadata or directory name.
# name = "{app_name}"
# Optional: runtime entrypoint override (relative to project root).
# main = "server/index.mjs"

# Build preset and artifact packaging.
[build]
preset = "bun"
# include = ["dist/**", ".output/**"]
# exclude = ["**/*.map"]
# assets = ["public", ".output/public"]

# Global environment variables applied to every environment.
# [vars]
# LOG_LEVEL = "info"
# API_BASE_URL = "https://api.example.com"

# Environment-specific variable overrides merged on top of [vars].
# [vars.production]
# LOG_LEVEL = "warn"
# API_BASE_URL = "https://api.example.com"

# [vars.staging]
# LOG_LEVEL = "debug"
# API_BASE_URL = "https://staging-api.example.com"

# Environment declarations. Deploy environments must define `route` or `routes`.
[envs.production]
route = "{app_name}.example.com"

# Development routes are optional; default is `{app_name}.tako.local`.
# [envs.development]
# route = "{app_name}.tako.local"

# Optional: use multiple routes instead of `route`.
# routes = ["{app_name}.example.com", "www.{app_name}.example.com"]

# Optional: env-local variables can be set directly in this section.
# LOG_FORMAT = "json"
# FEATURE_FLAG_NEW_CHECKOUT = "true"

# [envs.staging]
# route = "staging.{app_name}.example.com"
# routes = ["staging.{app_name}.example.com", "www.staging.{app_name}.example.com"]
# LOG_LEVEL = "debug"

# Default runtime settings for every mapped server.
# [servers]
# instances = 0
# port = 80
# idle_timeout = 300

# Per-server overrides. Section name must match `tako servers ls`.
# [servers.production]
# env = "production"
# instances = 2
# port = 8080
# idle_timeout = 300

# [servers.staging]
# env = "staging"
# instances = 1
# idle_timeout = 120
"#,
        app_name = app_name
    )
}

#[cfg(test)]
mod tests {
    use super::generate_template;

    #[test]
    fn init_template_keeps_only_minimal_options_uncommented() {
        let rendered = generate_template("demo-app", "bun");

        assert!(
            rendered
                .contains("# Stable app identifier used for deploy paths and local dev hostnames."),
            "expected template to explain app name identity semantics"
        );
        assert!(
            rendered.contains("# Set once and do not change after first deploy."),
            "expected template to warn that app name should remain stable"
        );
        assert!(
            rendered.contains("# name = \"demo-app\""),
            "expected app name to remain commented by default"
        );
        assert!(
            !rendered.contains("\nname = \"demo-app\"\n"),
            "expected app name not to be uncommented in minimal template"
        );
        assert!(
            rendered.contains("[envs.production]\nroute = \"demo-app.example.com\""),
            "expected production route to remain uncommented"
        );
        assert!(
            rendered.contains("# [envs.development]"),
            "expected development environment section to be optional/commented by default"
        );
        assert!(
            !rendered.contains("[envs.development]\nroute = \"demo-app.tako.local\""),
            "expected development route not to be uncommented in minimal template"
        );

        assert!(
            rendered.contains("[build]\npreset = \"bun\""),
            "expected build preset section to be present and uncommented"
        );
        assert!(
            rendered.contains("# main = \"server/index.mjs\""),
            "expected optional main entrypoint to be commented"
        );
        assert!(
            rendered.contains("# assets = [\"public\", \".output/public\"]"),
            "expected optional build assets list to be commented"
        );
        assert!(
            rendered.contains("# [vars]"),
            "expected vars section to be commented"
        );
        assert!(
            rendered.contains("# [servers]"),
            "expected server defaults section to be commented"
        );
        assert!(
            rendered.contains("# [servers.production]"),
            "expected per-server section to be commented"
        );
    }

    #[test]
    fn init_template_includes_reference_link_and_option_examples() {
        let rendered = generate_template("demo-app", "bun");

        assert!(
            rendered.contains("https://tako.sh/docs/tako-toml"),
            "expected link to tako.toml reference docs"
        );
        assert!(
            rendered
                .contains("# routes = [\"demo-app.example.com\", \"www.demo-app.example.com\"]"),
            "expected routes example in commented options"
        );
        assert!(
            rendered.contains("# include = [\"dist/**\", \".output/**\"]"),
            "expected build include example in commented options"
        );
        assert!(
            rendered.contains("# API_BASE_URL = \"https://api.example.com\""),
            "expected example for environment variables"
        );
        assert!(
            rendered.contains("# idle_timeout = 300"),
            "expected server idle timeout example"
        );
    }
}
