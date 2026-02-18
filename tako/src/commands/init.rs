use std::env::current_dir;
use std::fs;
use std::path::Path;

use crate::app::resolve_app_name;
use crate::build::{BuildAdapter, detect_build_adapter};
use crate::output;

const EMBEDDED_BUN_TANSTACK_START_PRESET_CONTENT: &str =
    include_str!("../../../presets/bun/tanstack-start.toml");

pub fn run(force: bool, runtime_override: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
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

    let detected_adapter = detect_build_adapter(&project_dir);
    let adapter = select_adapter_for_init(detected_adapter, runtime_override)?;
    let selected_preset = select_preset_for_adapter(adapter)?;
    let preset_default_main = selected_preset
        .as_deref()
        .and_then(|preset| embedded_preset_default_main(preset, adapter));
    let selected_preset_for_toml = selected_preset
        .as_deref()
        .filter(|preset| *preset != adapter.default_preset())
        .map(str::to_string);

    // Resolve app name
    let detected_app_name = resolve_app_name(&project_dir).unwrap_or_else(|_| "my-app".to_string());
    output::muted(
        "App name should be unique per server. Renaming later creates a new app; delete the old deployment manually with `tako delete`.",
    );
    let app_name = output::prompt_input(
        "App name (`name` in tako.toml; unique per server)",
        false,
        Some(&detected_app_name),
    )?;
    let default_production_route = format!("{}.example.com", app_name.trim());
    let production_route = output::prompt_input(
        "Production route (`[envs.production].route`)",
        false,
        Some(&default_production_route),
    )?;
    let inferred_main = adapter.infer_main_entrypoint(&project_dir);
    let main = if let Some(inferred_main) = inferred_main {
        if preset_default_main.as_deref() == Some(inferred_main.as_str()) {
            None
        } else {
            Some(inferred_main)
        }
    } else if preset_default_main.is_some() {
        None
    } else {
        let default_main = infer_default_main_entrypoint(&project_dir, adapter);
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
        Some(adapter.id()),
        selected_preset_for_toml.as_deref(),
    );

    fs::write(&tako_toml_path, template)?;

    output::success("Created tako.toml");

    output::section("Detected");
    output::step(&format!("Runtime: {}", adapter.id()));
    if let Some(preset_ref) = selected_preset_for_toml.as_deref() {
        output::step(&format!("Preset: {}", preset_ref));
    } else if selected_preset.as_deref() == Some(adapter.default_preset()) {
        output::step("Preset: runtime default (omitted in tako.toml)");
    } else {
        output::step("Preset: custom (unset in tako.toml)");
    }
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

fn select_adapter_for_init(
    detected_adapter: BuildAdapter,
    runtime_override: Option<&str>,
) -> std::io::Result<BuildAdapter> {
    if let Some(runtime_override) = runtime_override.map(str::trim).filter(|v| !v.is_empty()) {
        return BuildAdapter::from_id(runtime_override).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Invalid runtime '{}'; expected one of: bun, node, deno",
                    runtime_override
                ),
            )
        });
    }

    if !output::is_interactive() {
        return Ok(match detected_adapter {
            BuildAdapter::Unknown => BuildAdapter::Bun,
            other => other,
        });
    }

    let mut adapters = vec![BuildAdapter::Bun, BuildAdapter::Node, BuildAdapter::Deno];
    if detected_adapter != BuildAdapter::Unknown
        && let Some(index) = adapters
            .iter()
            .position(|adapter| *adapter == detected_adapter)
    {
        let detected = adapters.remove(index);
        adapters.insert(0, detected);
    }
    let options = adapters
        .into_iter()
        .map(|adapter| (adapter.id().to_string(), adapter))
        .collect();

    let description = if detected_adapter == BuildAdapter::Unknown {
        "Choose a runtime for default preset selection and entrypoint inference."
    } else {
        "Detected runtime is listed first. Choose another to override detection."
    };
    output::select(
        "Runtime (`runtime` in tako.toml)",
        Some(description),
        options,
    )
}

fn infer_default_main_entrypoint(project_dir: &Path, adapter: BuildAdapter) -> String {
    if let Some(main) = adapter.infer_main_entrypoint(project_dir) {
        return main;
    }

    const CANDIDATES: &[&str] = &[
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "src/index.ts",
        "src/index.tsx",
        "src/index.js",
        "src/index.jsx",
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

fn parse_preset_default_main(content: &str) -> Option<String> {
    let parsed: toml::Value = toml::from_str(content).ok()?;
    let main = parsed.get("main")?.as_str()?.trim();
    if main.is_empty() {
        None
    } else {
        Some(main.to_string())
    }
}

fn embedded_preset_default_main(preset_ref: &str, adapter: BuildAdapter) -> Option<String> {
    match preset_ref {
        "bun" | "bun/bun" => adapter.embedded_preset_default_main(),
        "bun/tanstack-start" => {
            parse_preset_default_main(EMBEDDED_BUN_TANSTACK_START_PRESET_CONTENT)
        }
        "node" | "node/node" | "deno" | "deno/deno" => adapter.embedded_preset_default_main(),
        _ => None,
    }
}

fn select_preset_for_adapter(adapter: BuildAdapter) -> std::io::Result<Option<String>> {
    if !output::is_interactive() {
        return Ok(Some(adapter.default_preset().to_string()));
    }

    let mut options: Vec<(String, Option<String>)> = Vec::new();
    match adapter {
        BuildAdapter::Bun => {
            options.push(("bun (base preset)".to_string(), Some("bun".to_string())));
            options.push((
                "bun/tanstack-start".to_string(),
                Some("bun/tanstack-start".to_string()),
            ));
        }
        BuildAdapter::Node => {
            options.push(("node (base preset)".to_string(), Some("node".to_string())));
        }
        BuildAdapter::Deno => {
            options.push(("deno (base preset)".to_string(), Some("deno".to_string())));
        }
        BuildAdapter::Unknown => {
            options.push(("bun (base preset)".to_string(), Some("bun".to_string())));
        }
    }
    options.push((
        "Custom preset reference (leave unset for now)".to_string(),
        None,
    ));

    output::select(
        "Build preset (`preset` in tako.toml)",
        Some("Choose a built-in preset or leave it unset and add your own later."),
        options,
    )
}

fn generate_template(
    app_name: &str,
    main: Option<&str>,
    production_route: &str,
    runtime: Option<&str>,
    preset_ref: Option<&str>,
) -> String {
    let main_line = if let Some(main) = main {
        format!(
            "# Required: runtime entrypoint used by `tako dev` and `tako deploy` (relative to project root).\nmain = \"{}\"",
            main
        )
    } else {
        "# Entrypoint comes from the selected preset default `main`.\n# main = \"index.ts\""
            .to_string()
    };
    let runtime_line = if let Some(runtime) = runtime {
        format!("runtime = \"{}\"", runtime)
    } else {
        "# runtime = \"bun\"".to_string()
    };
    let default_preset_comment = runtime.unwrap_or("bun");
    let explicit_preset = preset_ref.filter(|preset| *preset != default_preset_comment);
    let preset_line = if let Some(preset_ref) = explicit_preset {
        format!("preset = \"{}\"", preset_ref)
    } else {
        format!("# preset = \"{}\"", default_preset_comment)
    };
    format!(
        r#"# Tako configuration
# tako.toml reference: https://tako.sh/docs/tako-toml

# Stable app identifier used for deploy paths and local dev hostnames.
# Keep it unique per server. Renaming creates a new app path.
# If you rename it, delete the old deployment manually with `tako delete`.
name = "{app_name}"
{main_line}

# Build runtime and preset selection for runtime/build lifecycle defaults.
{runtime_line}
{preset_line}

# Artifact packaging options.
[build]
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

# Environment sections only define routes.
# Set environment variables in [vars] and [vars.<environment>].

# [envs.staging]
# route = "staging.{app_name}.example.com"
# routes = ["staging.{app_name}.example.com", "www.staging.{app_name}.example.com"]

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
        runtime_line = runtime_line,
        preset_line = preset_line,
        production_route = production_route
    )
}

#[cfg(test)]
mod tests {
    use super::{
        embedded_preset_default_main, generate_template, infer_default_main_entrypoint,
        select_adapter_for_init,
    };
    use crate::build::BuildAdapter;
    use tempfile::TempDir;

    #[test]
    fn init_template_keeps_only_minimal_options_uncommented() {
        let rendered = generate_template(
            "demo-app",
            Some("server/index.mjs"),
            "demo-app.example.com",
            Some("bun"),
            Some("bun"),
        );

        assert!(
            rendered
                .contains("# Stable app identifier used for deploy paths and local dev hostnames."),
            "expected template to explain app name identity semantics"
        );
        assert!(
            rendered.contains("# Keep it unique per server. Renaming creates a new app path."),
            "expected template to warn that app names must be unique"
        );
        assert!(
            rendered.contains(
                "# If you rename it, delete the old deployment manually with `tako delete`."
            ),
            "expected template to explain rename cleanup behavior"
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
            rendered.contains("runtime = \"bun\"\n# preset = \"bun\""),
            "expected base runtime preset to be omitted/commented"
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
        let rendered = generate_template(
            "demo-app",
            Some("server/index.mjs"),
            "demo-app.example.com",
            Some("bun"),
            Some("bun"),
        );

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
            infer_default_main_entrypoint(temp.path(), BuildAdapter::Unknown),
            "server/index.ts"
        );
    }

    #[test]
    fn infer_default_main_entrypoint_prefers_root_js_extension_order_before_src() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("index.jsx"), "export default {};").unwrap();
        std::fs::write(temp.path().join("src/index.ts"), "export {};").unwrap();

        assert_eq!(
            infer_default_main_entrypoint(temp.path(), BuildAdapter::Unknown),
            "index.jsx"
        );
    }

    #[test]
    fn infer_default_main_entrypoint_supports_tsx_candidates() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/index.tsx"), "export default {};").unwrap();

        assert_eq!(
            infer_default_main_entrypoint(temp.path(), BuildAdapter::Unknown),
            "src/index.tsx"
        );
    }

    #[test]
    fn infer_default_main_entrypoint_falls_back_when_no_candidate_exists() {
        let temp = TempDir::new().unwrap();
        assert_eq!(
            infer_default_main_entrypoint(temp.path(), BuildAdapter::Unknown),
            "index.ts"
        );
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

        assert_eq!(
            infer_default_main_entrypoint(temp.path(), BuildAdapter::Node),
            "app/server.ts"
        );
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
            infer_default_main_entrypoint(temp.path(), BuildAdapter::Node),
            "server/index.ts"
        );
    }

    #[test]
    fn init_template_can_omit_main_when_preset_provides_default() {
        let rendered = generate_template(
            "demo-app",
            None,
            "demo-app.example.com",
            Some("bun"),
            Some("bun"),
        );
        assert!(rendered.contains("# Entrypoint comes from the selected preset default `main`."));
        assert!(!rendered.contains("\nmain = \""));
    }

    #[test]
    fn init_template_uses_prompted_production_route() {
        let rendered = generate_template(
            "demo-app",
            Some("server/index.mjs"),
            "api.demo-app.com",
            Some("bun"),
            Some("bun"),
        );
        assert!(rendered.contains("[envs.production]\nroute = \"api.demo-app.com\""));
        assert!(!rendered.contains("[envs.production]\nroute = \"demo-app.example.com\""));
    }

    #[test]
    fn init_template_can_leave_preset_unset() {
        let rendered =
            generate_template("demo-app", None, "demo-app.example.com", Some("node"), None);
        assert!(rendered.contains("runtime = \"node\"\n# preset = \"node\""));
    }

    #[test]
    fn init_template_writes_selected_build_adapter() {
        let rendered = generate_template(
            "demo-app",
            None,
            "demo-app.example.com",
            Some("bun"),
            Some("bun"),
        );
        assert!(rendered.contains("runtime = \"bun\""));
    }

    #[test]
    fn embedded_bun_preset_default_main_is_set() {
        assert_eq!(
            embedded_preset_default_main("bun", BuildAdapter::Bun),
            Some("src/index.ts".to_string())
        );
    }

    #[test]
    fn embedded_bun_tanstack_start_preset_default_main_is_set() {
        assert_eq!(
            embedded_preset_default_main("bun/tanstack-start", BuildAdapter::Bun),
            Some("dist/server/tako-entry.mjs".to_string())
        );
    }

    #[test]
    fn select_adapter_for_init_uses_override_when_provided() {
        assert_eq!(
            select_adapter_for_init(BuildAdapter::Node, Some("deno")).unwrap(),
            BuildAdapter::Deno
        );
    }

    #[test]
    fn select_adapter_for_init_rejects_unknown_override() {
        let err = select_adapter_for_init(BuildAdapter::Node, Some("python")).unwrap_err();
        assert!(err.to_string().contains("Invalid runtime"));
    }

    #[test]
    fn select_adapter_for_init_defaults_unknown_detection_to_bun_non_interactive() {
        assert_eq!(
            select_adapter_for_init(BuildAdapter::Unknown, None).unwrap(),
            BuildAdapter::Bun
        );
    }
}
