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
        r#"# Tako Configuration
# https://github.com/anthropics/tako

[tako]
# Application name (auto-detected: {app_name})
name = "{app_name}"

# Build command (optional, run before deployment)
# build = "bun run build"

# ============================================================================
# Environment Variables
# ============================================================================
# Global variables (applied to all environments)
[vars]
# LOG_LEVEL = "info"

# Per-environment variable overrides
# [vars.production]
# LOG_LEVEL = "warn"

# [vars.staging]
# LOG_LEVEL = "debug"

# ============================================================================
# Environments
# ============================================================================
# Each environment defines routes and can be deployed to different servers

[envs.production]
# Single route
route = "api.example.com"
# Or multiple routes:
# routes = ["api.example.com", "*.api.example.com"]

[envs.staging]
route = "staging.example.com"

# ============================================================================
# Server Configuration
# ============================================================================
# Default settings for all servers
[servers]
instances = 0      # 0 = on-demand scaling (cold start when needed)
port = 80          # Application port
idle_timeout = 300 # Seconds before idle shutdown (5 minutes)

# Per-server overrides (server must exist in ~/.tako/config.toml [[servers]])
# [servers.prod-1]
# env = "production"
# instances = 2      # Always keep 2 instances running

# [servers.staging-1]
# env = "staging"
"#,
        app_name = app_name
    )
}
