use std::env::current_dir;
use std::fs;
use std::path::Path;

use crate::app::resolve_app_name;
use crate::output;

const EMBEDDED_BUN_PRESET_CONTENT: &str = include_str!("../../../presets/bun.toml");

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

    let runtime_name = "bun";

    // Resolve app name
    let detected_app_name = resolve_app_name(&project_dir).unwrap_or_else(|_| "my-app".to_string());
    let app_name = output::prompt_input(
        "App name (`name` in tako.toml)",
        false,
        Some(&detected_app_name),
    )?;
    let default_production_route = format!("{}.example.com", app_name.trim());
    let production_route = output::prompt_input(
        "Production route (`[envs.production].route`)",
        false,
        Some(&default_production_route),
    )?;
    let preset_default_main = embedded_bun_preset_default_main();
    let main = if preset_default_main.is_some() {
        None
    } else {
        let default_main = infer_default_main_entrypoint(&project_dir);
        Some(output::prompt_input(
            "Deploy/dev entrypoint (`main` in tako.toml)",
            false,
            Some(&default_main),
        )?)
    };

    // Generate tako.toml
    let template = generate_template(
        app_name.trim(),
        main.as_deref().map(str::trim),
        production_route.trim(),
    );

    fs::write(&tako_toml_path, template)?;

    output::success("Created tako.toml");

    output::section("Detected");
    output::step(&format!("Runtime: {}", runtime_name));
    output::step(&format!("App name: {}", app_name.trim()));
    output::step(&format!("Production route: {}", production_route.trim()));
    if let Some(main) = main.as_deref() {
        output::step(&format!("Main: {}", main.trim()));
    } else if let Some(main) = preset_default_main.as_deref() {
        output::step(&format!("Main: {} (from preset)", main));
    }

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

fn infer_default_main_entrypoint(project_dir: &Path) -> String {
    if let Some(main) = infer_main_from_package_json(project_dir) {
        return main;
    }

    const CANDIDATES: &[&str] = &[
        "index.ts",
        "index.js",
        "src/index.ts",
        "src/index.js",
        "server/index.mjs",
        "server/index.ts",
        "server/index.js",
        "main.py",
        "main.rb",
        "main.go",
    ];

    for candidate in CANDIDATES {
        if project_dir.join(candidate).is_file() {
            return (*candidate).to_string();
        }
    }

    "index.ts".to_string()
}

fn infer_main_from_package_json(project_dir: &Path) -> Option<String> {
    let package_json_path = project_dir.join("package.json");
    if !package_json_path.is_file() {
        return None;
    }
    let raw = fs::read_to_string(package_json_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let main = parsed.get("main")?.as_str()?.trim();
    if main.is_empty() {
        return None;
    }

    let normalized = main.replace('\\', "/");
    let normalized = normalized.trim_start_matches("./").to_string();
    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains("..") {
        return None;
    }
    if project_dir.join(&normalized).is_file() {
        Some(normalized)
    } else {
        None
    }
}

fn embedded_bun_preset_default_main() -> Option<String> {
    let parsed: toml::Value = toml::from_str(EMBEDDED_BUN_PRESET_CONTENT).ok()?;
    let main = parsed.get("main")?.as_str()?.trim();
    if main.is_empty() {
        None
    } else {
        Some(main.to_string())
    }
}

fn generate_template(app_name: &str, main: Option<&str>, production_route: &str) -> String {
    let main_line = if let Some(main) = main {
        format!(
            "# Required: runtime entrypoint used by `tako dev` and `tako deploy` (relative to project root).\nmain = \"{}\"",
            main
        )
    } else {
        "# Entrypoint comes from the selected preset default `main`.\n# main = \"index.ts\""
            .to_string()
    };
    format!(
        r#"# Tako configuration
# tako.toml reference: https://tako.sh/docs/tako-toml

# Required stable app identifier used for deploy paths and local dev hostnames.
# Set once and do not change after first deploy.
name = "{app_name}"
{main_line}

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
route = "{production_route}"

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
        app_name = app_name,
        main_line = main_line,
        production_route = production_route
    )
}

#[cfg(test)]
mod tests {
    use super::{
        embedded_bun_preset_default_main, generate_template, infer_default_main_entrypoint,
    };
    use tempfile::TempDir;

    #[test]
    fn init_template_keeps_only_minimal_options_uncommented() {
        let rendered =
            generate_template("demo-app", Some("server/index.mjs"), "demo-app.example.com");

        assert!(
            rendered.contains(
                "# Required stable app identifier used for deploy paths and local dev hostnames."
            ),
            "expected template to explain app name identity semantics"
        );
        assert!(
            rendered.contains("# Set once and do not change after first deploy."),
            "expected template to warn that app name should remain stable"
        );
        assert!(
            rendered.contains("\nname = \"demo-app\"\n"),
            "expected app name to be uncommented in minimal template"
        );
        assert!(
            !rendered.contains("# name = \"demo-app\""),
            "expected app name commented example to be removed"
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
            rendered.contains("main = \"server/index.mjs\""),
            "expected required main entrypoint to be uncommented"
        );
        assert!(
            !rendered.contains("# main = \"server/index.mjs\""),
            "expected legacy commented main example to be removed"
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
        let rendered =
            generate_template("demo-app", Some("server/index.mjs"), "demo-app.example.com");

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

    #[test]
    fn infer_default_main_entrypoint_prefers_existing_file() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("server")).unwrap();
        std::fs::write(temp.path().join("server/index.ts"), "export {};").unwrap();
        assert_eq!(
            infer_default_main_entrypoint(temp.path()),
            "server/index.ts"
        );
    }

    #[test]
    fn infer_default_main_entrypoint_falls_back_when_no_candidate_exists() {
        let temp = TempDir::new().unwrap();
        assert_eq!(infer_default_main_entrypoint(temp.path()), "index.ts");
    }

    #[test]
    fn infer_default_main_entrypoint_uses_package_json_main_when_file_exists() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("app")).unwrap();
        std::fs::write(temp.path().join("app/server.ts"), "export {};").unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"name":"demo","main":"app/server.ts"}"#,
        )
        .unwrap();

        assert_eq!(infer_default_main_entrypoint(temp.path()), "app/server.ts");
    }

    #[test]
    fn infer_default_main_entrypoint_ignores_package_json_main_when_file_is_missing() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("server")).unwrap();
        std::fs::write(temp.path().join("server/index.ts"), "export {};").unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"name":"demo","main":"dist/index.js"}"#,
        )
        .unwrap();

        assert_eq!(
            infer_default_main_entrypoint(temp.path()),
            "server/index.ts"
        );
    }

    #[test]
    fn init_template_can_omit_main_when_preset_provides_default() {
        let rendered = generate_template("demo-app", None, "demo-app.example.com");
        assert!(rendered.contains("# Entrypoint comes from the selected preset default `main`."));
        assert!(!rendered.contains("\nmain = \""));
    }

    #[test]
    fn init_template_uses_prompted_production_route() {
        let rendered = generate_template("demo-app", Some("server/index.mjs"), "api.demo-app.com");
        assert!(rendered.contains("[envs.production]\nroute = \"api.demo-app.com\""));
        assert!(!rendered.contains("[envs.production]\nroute = \"demo-app.example.com\""));
    }

    #[test]
    fn embedded_bun_preset_default_main_is_set() {
        assert_eq!(
            embedded_bun_preset_default_main(),
            Some("src/index.ts".to_string())
        );
    }
}
