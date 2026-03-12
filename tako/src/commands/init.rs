use std::env::current_dir;
use std::fs;
use std::path::Path;

use crate::app::resolve_app_name;
use crate::build::js;
use crate::build::{
    BuildAdapter, FamilyPresetDefinition, PresetFamily, detect_build_adapter,
    load_available_family_preset_definitions,
};
use crate::config::TakoToml;
use crate::output;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;
    let tako_toml_path = project_dir.join("tako.toml");

    // Load existing config for pre-filling defaults
    let existing = if tako_toml_path.exists() {
        TakoToml::load_from_file(&tako_toml_path).ok()
    } else {
        None
    };

    // Check if tako.toml already exists — prompt to overwrite in interactive mode
    if existing.is_some() {
        if !output::is_interactive()
            || !output::confirm(
                &format!(
                    "Configuration file {} already exists. Overwrite?",
                    output::bold("tako.toml")
                ),
                false,
            )?
        {
            return Ok(());
        }
    }

    let detected_adapter = detect_build_adapter(&project_dir);

    // Non-interactive: skip wizard, use defaults
    if !output::is_interactive() {
        return run_non_interactive(
            &project_dir,
            &tako_toml_path,
            detected_adapter,
            existing.as_ref(),
        );
    }

    // Interactive wizard with state machine for ESC go-back
    let mut wizard = output::Wizard::new()
        .with_fields(&[
            ("Application name", false),
            ("Runtime", false),
            ("Build preset", false),
            ("Entrypoint", true), // subsection — hidden until custom preset
            ("Assets", true),     // subsection
            ("Exclude", true),    // subsection
            ("Production route", false),
        ])
        .with_confirmation();
    let mut step = 0usize;
    let mut step_history: Vec<usize> = Vec::new();

    // Cached family presets (keyed by adapter to avoid re-fetching)
    let mut family_presets_cache: Option<(BuildAdapter, Vec<FamilyPresetDefinition>)> = None;

    // Accumulated values — pre-filled from existing config when overwriting
    let mut adapter = existing
        .as_ref()
        .and_then(|c| c.runtime.as_deref())
        .and_then(BuildAdapter::from_id)
        .unwrap_or(detected_adapter);
    let mut selected_preset: Option<String> = existing.as_ref().and_then(|c| c.preset.clone());
    let mut main_entry: Option<String> = existing.as_ref().and_then(|c| c.main.clone());
    let mut assets: Vec<String> = existing
        .as_ref()
        .map(|c| c.build.assets.clone())
        .unwrap_or_default();
    let mut excludes: Vec<String> = existing
        .as_ref()
        .map(|c| c.build.exclude.clone())
        .unwrap_or_default();
    let mut app_name = existing
        .as_ref()
        .and_then(|c| c.name.clone())
        .unwrap_or_default();
    let mut production_route = existing
        .as_ref()
        .and_then(|c| c.envs.get("production").and_then(|e| e.route.clone()))
        .unwrap_or_default();

    // Derived state
    let mut is_custom = selected_preset.is_none();

    // Pre-populate wizard from existing config
    if existing.is_some() {
        if !app_name.is_empty() {
            wizard.set("Application name", &app_name);
        }
        wizard.set("Runtime", adapter.id());
        if let Some(ref preset) = selected_preset {
            wizard.set("Build preset", preset);
        } else {
            wizard.set("Build preset", "custom");
        }
        if is_custom {
            wizard.set_visible("Entrypoint", true);
            wizard.set_visible("Assets", true);
            wizard.set_visible("Exclude", true);
            if let Some(ref main) = main_entry {
                wizard.set("Entrypoint", main);
            }
            if !assets.is_empty() {
                wizard.set("Assets", &assets.join(", "));
            }
            if !excludes.is_empty() {
                wizard.set("Exclude", &excludes.join(", "));
            }
        }
        if !production_route.is_empty() {
            wizard.set("Production route", &production_route);
        }
    }

    loop {
        match step {
            // Step 0: App name
            0 => {
                let default_app_name = if !app_name.is_empty() {
                    app_name.clone()
                } else {
                    existing
                        .as_ref()
                        .and_then(|c| c.name.clone())
                        .unwrap_or_else(|| {
                            resolve_app_name(&project_dir).unwrap_or_else(|_| "my-app".to_string())
                        })
                };
                match wizard.input(
                    "Application name",
                    Some(&default_app_name),
                    Some("Name cannot be changed after the first deployment."),
                ) {
                    Ok(v) => {
                        app_name = v;
                        step_history.push(0);
                        step = 1;
                    }
                    Err(e) if output::is_wizard_back(&e) => return Ok(()),
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 1: Runtime (pre-filled with detected value)
            1 => {
                let adapters = [BuildAdapter::Bun, BuildAdapter::Node, BuildAdapter::Deno];
                let default_index = adapters.iter().position(|a| *a == adapter).unwrap_or(0);
                let options: Vec<(String, BuildAdapter)> =
                    adapters.iter().map(|&a| (a.id().to_string(), a)).collect();
                let hints: Vec<&str> = adapters
                    .iter()
                    .map(|&a| {
                        if a == detected_adapter && detected_adapter != BuildAdapter::Unknown {
                            "detected"
                        } else {
                            ""
                        }
                    })
                    .collect();
                match wizard.select(
                    "Runtime",
                    "Choose a runtime:",
                    options,
                    &hints,
                    default_index,
                ) {
                    Ok(a) => {
                        adapter = a;
                        step_history.push(1);
                        step = 2;
                    }
                    Err(e) if output::is_wizard_back(&e) => {
                        if let Some(prev) = step_history.pop() {
                            step = prev;
                        } else {
                            return Ok(());
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 2: Build preset + compute derived state
            2 => {
                let family_presets = match &family_presets_cache {
                    Some((cached, presets)) if *cached == adapter => presets.clone(),
                    _ => {
                        let presets = fetch_family_presets_for_adapter(adapter)?;
                        family_presets_cache = Some((adapter, presets.clone()));
                        presets
                    }
                };
                let family_preset_names: Vec<String> =
                    family_presets.iter().map(|p| p.name.clone()).collect();
                let existing_preset_ref = existing.as_ref().and_then(|c| c.preset.as_deref());

                if let Some(options) = build_preset_selection_options(adapter, &family_preset_names)
                {
                    let default_index = selected_preset
                        .as_deref()
                        .and_then(|sp| options.iter().position(|(_, v)| v.as_deref() == Some(sp)))
                        .or_else(|| {
                            existing_preset_ref.and_then(|ep| {
                                options.iter().position(|(_, v)| v.as_deref() == Some(ep))
                            })
                        })
                        .unwrap_or(0);
                    match wizard.select(
                        "Build preset",
                        "Choose a build preset:",
                        options,
                        &[],
                        default_index,
                    ) {
                        Ok(sp) => {
                            selected_preset = sp;
                        }
                        Err(e) if output::is_wizard_back(&e) => {
                            if let Some(prev) = step_history.pop() {
                                step = prev;
                            } else {
                                return Ok(());
                            }
                            continue;
                        }
                        Err(e) => return Err(e.into()),
                    }
                } else {
                    selected_preset = Some(adapter.default_preset().to_string());
                }

                // Compute derived state
                is_custom = selected_preset.is_none();
                let preset_dm = selected_preset
                    .as_deref()
                    .and_then(|preset| preset_default_main(preset, adapter, &family_presets));
                let inferred_main = adapter.infer_main_entrypoint(&project_dir);

                step_history.push(2);

                if is_custom {
                    wizard.set_visible("Entrypoint", true);
                    wizard.set_visible("Assets", true);
                    wizard.set_visible("Exclude", true);
                    step = 3; // entrypoint prompt
                } else if let Some(ref inferred) = inferred_main {
                    main_entry = if preset_dm.as_deref() == Some(inferred.as_str()) {
                        None
                    } else {
                        Some(inferred.clone())
                    };
                    wizard.set_visible("Entrypoint", false);
                    wizard.set_visible("Assets", false);
                    wizard.set_visible("Exclude", false);
                    step = 6; // skip to production route
                } else if preset_dm.is_some() {
                    main_entry = None;
                    wizard.set_visible("Entrypoint", false);
                    wizard.set_visible("Assets", false);
                    wizard.set_visible("Exclude", false);
                    step = 6;
                } else {
                    wizard.set_visible("Entrypoint", true);
                    wizard.set_visible("Assets", false);
                    wizard.set_visible("Exclude", false);
                    step = 3; // need entrypoint prompt
                }
            }
            // Step 3: Entrypoint
            3 => {
                let default_main = main_entry
                    .clone()
                    .or_else(|| existing.as_ref().and_then(|c| c.main.clone()))
                    .or_else(|| adapter.infer_main_entrypoint(&project_dir))
                    .unwrap_or_else(|| infer_default_main_entrypoint(&project_dir, adapter));
                match wizard.input("Entrypoint", Some(&default_main), None) {
                    Ok(v) => {
                        main_entry = Some(v);
                        step_history.push(3);
                        if is_custom {
                            step = 4;
                        } else {
                            step = 6;
                        }
                    }
                    Err(e) if output::is_wizard_back(&e) => {
                        if let Some(prev) = step_history.pop() {
                            step = prev;
                        } else {
                            return Ok(());
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 4: Assets (custom only)
            4 => {
                let existing_assets = existing
                    .as_ref()
                    .map(|c| c.build.assets.clone())
                    .unwrap_or_default();
                let prev_assets = if !assets.is_empty() {
                    &assets
                } else {
                    &existing_assets
                };
                match prompt_assets(&mut wizard, prev_assets) {
                    Ok(collected) => {
                        if !collected.is_empty() {
                            wizard.set("Assets", &collected.join(", "));
                        }
                        assets = collected;
                        step_history.push(4);
                        step = 5;
                    }
                    Err(e) if output::is_wizard_back(&e) => {
                        if let Some(prev) = step_history.pop() {
                            step = prev;
                        } else {
                            return Ok(());
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 5: Excludes (custom only)
            5 => {
                let existing_excludes = existing
                    .as_ref()
                    .map(|c| c.build.exclude.clone())
                    .unwrap_or_default();
                let prev_excludes = if !excludes.is_empty() {
                    &excludes
                } else {
                    &existing_excludes
                };
                match prompt_excludes(&mut wizard, prev_excludes) {
                    Ok(collected) => {
                        if !collected.is_empty() {
                            wizard.set("Exclude", &collected.join(", "));
                        }
                        excludes = collected;
                        step_history.push(5);
                        step = 6;
                    }
                    Err(e) if output::is_wizard_back(&e) => {
                        if let Some(prev) = step_history.pop() {
                            step = prev;
                        } else {
                            return Ok(());
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 6: Production route
            6 => {
                let default_route = if !production_route.is_empty() {
                    production_route.clone()
                } else {
                    existing
                        .as_ref()
                        .and_then(|c| c.envs.get("production").and_then(|e| e.route.clone()))
                        .unwrap_or_else(|| format!("{}.example.com", app_name.trim()))
                };
                match wizard.input("Production route", Some(&default_route), None) {
                    Ok(v) => {
                        production_route = v;
                        step_history.push(6);
                        step = 7;
                    }
                    Err(e) if output::is_wizard_back(&e) => {
                        if let Some(prev) = step_history.pop() {
                            step = prev;
                        } else {
                            return Ok(());
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            // Step 7: Confirm
            _ => match wizard.finish() {
                Ok(true) => break,
                Ok(false) => {
                    step_history.clear();
                    step = 0;
                }
                Err(e) if output::is_wizard_back(&e) => {
                    if let Some(prev) = step_history.pop() {
                        step = prev;
                    }
                }
                Err(e) => return Err(e.into()),
            },
        }
    }

    let selected_preset_for_toml = selected_preset
        .as_deref()
        .filter(|preset| *preset != adapter.default_preset())
        .map(str::to_string);

    // Generate tako.toml
    let template = generate_template(
        app_name.trim(),
        main_entry.as_deref().map(str::trim),
        &sanitize_route(&production_route),
        Some(adapter.id()),
        selected_preset_for_toml.as_deref(),
        &assets,
        &excludes,
    );

    fs::write(&tako_toml_path, template)?;

    output::success("Created tako.toml");

    if js::write_types(&project_dir)? {
        output::success("Created tako.d.ts");
    }

    output::heading("Next steps");
    output::step(&format!(
        "1. Edit {} to set environment variables and more",
        output::strong("tako.toml")
    ));
    output::step(&format!(
        "2. Run {} to add deployment servers",
        output::strong("tako servers add")
    ));
    output::step(&format!(
        "3. Run {} to add secrets",
        output::strong("tako secrets set")
    ));
    output::step(&format!(
        "4. Run {} to deploy your app",
        output::strong("tako deploy")
    ));

    Ok(())
}

fn resolve_adapter(detected_adapter: BuildAdapter, existing: Option<&TakoToml>) -> BuildAdapter {
    let preferred = existing
        .and_then(|c| c.runtime.as_deref())
        .and_then(BuildAdapter::from_id)
        .unwrap_or(detected_adapter);
    match preferred {
        BuildAdapter::Unknown => BuildAdapter::Bun,
        other => other,
    }
}

fn run_non_interactive(
    project_dir: &Path,
    tako_toml_path: &Path,
    detected_adapter: BuildAdapter,
    existing: Option<&TakoToml>,
) -> Result<(), Box<dyn std::error::Error>> {
    let adapter = resolve_adapter(detected_adapter, existing);
    let preset = adapter.default_preset().to_string();
    let preset_dm = preset_default_main(&preset, adapter, &[]);

    let inferred_main = adapter.infer_main_entrypoint(project_dir);
    let main = if let Some(inferred) = inferred_main {
        if preset_dm.as_deref() == Some(inferred.as_str()) {
            None
        } else {
            Some(inferred)
        }
    } else if preset_dm.is_some() {
        None
    } else {
        Some(
            existing
                .and_then(|c| c.main.clone())
                .unwrap_or_else(|| infer_default_main_entrypoint(project_dir, adapter)),
        )
    };

    let app_name = existing
        .and_then(|c| c.name.clone())
        .unwrap_or_else(|| resolve_app_name(project_dir).unwrap_or_else(|_| "my-app".to_string()));

    let production_route = existing
        .and_then(|c| c.envs.get("production").and_then(|e| e.route.clone()))
        .unwrap_or_else(|| format!("{}.example.com", app_name.trim()));

    let template = generate_template(
        app_name.trim(),
        main.as_deref().map(str::trim),
        &sanitize_route(&production_route),
        Some(adapter.id()),
        None,
        &[],
        &[],
    );

    fs::write(tako_toml_path, template)?;
    output::success("Created tako.toml");

    if js::write_types(project_dir)? {
        output::success("Created tako.d.ts");
    }

    Ok(())
}

/// Strip http(s):// prefix and trailing slash from a route hostname.
fn sanitize_route(route: &str) -> String {
    let s = route.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    s.trim_end_matches('/').to_string()
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

fn preset_default_main(
    preset_ref: &str,
    adapter: BuildAdapter,
    family_presets: &[FamilyPresetDefinition],
) -> Option<String> {
    match preset_ref {
        "bun" | "node" | "deno" => adapter.embedded_preset_default_main(),
        _ => family_presets
            .iter()
            .find(|preset| preset.name == preset_ref)
            .and_then(|preset| preset.main.clone()),
    }
}

fn fetch_family_presets_for_adapter(
    adapter: BuildAdapter,
) -> std::io::Result<Vec<FamilyPresetDefinition>> {
    if !output::is_interactive() {
        return Ok(Vec::new());
    }

    let family = adapter.preset_family();
    if family == PresetFamily::Unknown {
        return Ok(Vec::new());
    }

    let runtime = tokio::runtime::Runtime::new().map_err(|e| {
        std::io::Error::other(format!("Failed to initialize preset fetch runtime: {e}"))
    })?;
    let fetched = output::with_spinner_simple("Fetching presets", || {
        runtime.block_on(load_available_family_preset_definitions(family))
    });

    match fetched {
        Ok(presets) => Ok(normalize_family_preset_definitions(adapter, presets)),
        Err(err) => {
            output::warning(&format!(
                "Failed to fetch presets ({}). Using {} base preset.",
                err,
                output::strong(adapter.default_preset())
            ));
            Ok(Vec::new())
        }
    }
}

fn normalize_family_preset_definitions(
    adapter: BuildAdapter,
    preset_definitions: Vec<FamilyPresetDefinition>,
) -> Vec<FamilyPresetDefinition> {
    let base = adapter.default_preset();
    let mut normalized = Vec::new();
    for preset in preset_definitions {
        let trimmed = preset.name.trim();
        if trimmed.is_empty() || trimmed == base {
            continue;
        }
        if normalized
            .iter()
            .any(|existing: &FamilyPresetDefinition| existing.name == trimmed)
        {
            continue;
        }
        normalized.push(FamilyPresetDefinition {
            name: trimmed.to_string(),
            main: preset.main,
        });
    }
    normalized
}

fn build_preset_selection_options(
    _adapter: BuildAdapter,
    family_presets: &[String],
) -> Option<Vec<(String, Option<String>)>> {
    if family_presets.is_empty() {
        return None;
    }

    let mut options: Vec<(String, Option<String>)> = Vec::with_capacity(family_presets.len() + 1);
    for preset in family_presets {
        options.push((preset.clone(), Some(preset.clone())));
    }
    options.push(("custom".to_string(), None));

    Some(options)
}

fn prompt_assets(wizard: &mut output::Wizard, existing: &[String]) -> std::io::Result<Vec<String>> {
    let mut assets = Vec::new();
    for existing_asset in existing.iter() {
        match output::TextField::new("Asset directory")
            .optional()
            .with_default(existing_asset)
            .prompt()
        {
            Ok(value) => {
                wizard.track_line();
                if value.is_empty() {
                    return Ok(assets);
                }
                assets.push(value);
            }
            Err(e) if output::is_wizard_back(&e) => {
                if assets.is_empty() {
                    return Err(e);
                }
                return Ok(assets);
            }
            Err(e) => return Err(e),
        }
    }
    loop {
        match output::TextField::new("Asset directory")
            .optional()
            .prompt()
        {
            Ok(value) => {
                wizard.track_line();
                if value.is_empty() {
                    return Ok(assets);
                }
                assets.push(value);
            }
            Err(e) if output::is_wizard_back(&e) => {
                if assets.is_empty() {
                    return Err(e);
                }
                return Ok(assets);
            }
            Err(e) => return Err(e),
        }
    }
}

fn prompt_excludes(
    wizard: &mut output::Wizard,
    existing: &[String],
) -> std::io::Result<Vec<String>> {
    let mut excludes = Vec::new();
    for existing_exclude in existing.iter() {
        match output::TextField::new("Exclude pattern")
            .optional()
            .with_default(existing_exclude)
            .prompt()
        {
            Ok(value) => {
                wizard.track_line();
                if value.is_empty() {
                    return Ok(excludes);
                }
                excludes.push(value);
            }
            Err(e) if output::is_wizard_back(&e) => {
                if excludes.is_empty() {
                    return Err(e);
                }
                return Ok(excludes);
            }
            Err(e) => return Err(e),
        }
    }
    loop {
        match output::TextField::new("Exclude pattern")
            .optional()
            .prompt()
        {
            Ok(value) => {
                wizard.track_line();
                if value.is_empty() {
                    return Ok(excludes);
                }
                excludes.push(value);
            }
            Err(e) if output::is_wizard_back(&e) => {
                if excludes.is_empty() {
                    return Err(e);
                }
                return Ok(excludes);
            }
            Err(e) => return Err(e),
        }
    }
}

fn generate_template(
    app_name: &str,
    main: Option<&str>,
    production_route: &str,
    runtime: Option<&str>,
    preset_ref: Option<&str>,
    assets: &[String],
    excludes: &[String],
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
    let preset_example = match runtime {
        Some("bun") => "tanstack-start",
        Some("node") => "my-node-preset",
        Some("deno") => "my-deno-preset",
        _ => "my-preset",
    };
    let preset_line = if let Some(preset_ref) = preset_ref {
        format!("preset = \"{}\"", preset_ref)
    } else {
        format!("# preset = \"{}\"", preset_example)
    };
    let assets_line = if assets.is_empty() {
        "# assets = [\"public\", \".output/public\"]".to_string()
    } else {
        let items: Vec<String> = assets.iter().map(|a| format!("\"{}\"", a)).collect();
        format!("assets = [{}]", items.join(", "))
    };
    let exclude_line = if excludes.is_empty() {
        "# exclude = [\"**/*.map\"]".to_string()
    } else {
        let items: Vec<String> = excludes.iter().map(|e| format!("\"{}\"", e)).collect();
        format!("exclude = [{}]", items.join(", "))
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
{exclude_line}
{assets_line}
# [[build.stages]]
# name = "frontend-assets"
# working_dir = "frontend"
# install = "bun install"
# run = "bun run build"

# Global environment variables applied to every environment.
# [vars]
# TAKO_APP_LOG_LEVEL = "info"
# API_BASE_URL = "https://api.example.com"

# Environment-specific variable overrides merged on top of [vars].
# [vars.production]
# TAKO_APP_LOG_LEVEL = "warn"
# API_BASE_URL = "https://api.example.com"

# [vars.staging]
# TAKO_APP_LOG_LEVEL = "debug"
# API_BASE_URL = "https://staging-api.example.com"

# Environment declarations. Deploy environments must define `route` or `routes`.
[envs.production]
route = "{production_route}"

# Development routes are optional; default is `{app_name}.tako.test`.
# [envs.development]
# route = "{app_name}.tako.test"

# Optional: use multiple routes instead of `route`.
# routes = ["{app_name}.example.com", "www.{app_name}.example.com"]

# Environment sections define routes, server membership, and idle scale-down.
# Set environment variables in [vars] and [vars.<environment>].

# [envs.staging]
# route = "staging.{app_name}.example.com"
# routes = ["staging.{app_name}.example.com", "www.staging.{app_name}.example.com"]
# servers = ["production"]
# idle_timeout = 300

# [envs.staging]
# route = "staging.{app_name}.example.com"
# servers = ["staging"]
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
        build_preset_selection_options, generate_template, infer_default_main_entrypoint,
        normalize_family_preset_definitions, preset_default_main, resolve_adapter,
    };
    use crate::build::{BuildAdapter, FamilyPresetDefinition};
    use tempfile::TempDir;

    #[test]
    fn init_template_keeps_only_minimal_options_uncommented() {
        let rendered = generate_template(
            "demo-app",
            Some("server/index.mjs"),
            "demo-app.example.com",
            Some("bun"),
            None,
            &[],
            &[],
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
            !rendered.contains("[envs.development]\nroute = \"demo-app.tako\""),
            "expected development route not to be uncommented in minimal template"
        );

        assert!(
            rendered.contains("runtime = \"bun\"\n# preset = \"tanstack-start\""),
            "expected base runtime preset to be omitted/commented"
        );
        assert!(
            rendered.contains("main = \"server/index.mjs\""),
            "expected required main entrypoint to be uncommented"
        );
        assert!(
            !rendered.contains("# main = \"server/index.mjs\""),
            "expected commented main example to be removed"
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
            rendered.contains("# servers = [\"production\"]"),
            "expected env-local server list example to be commented"
        );
        assert!(
            rendered.contains("# idle_timeout = 300"),
            "expected env-local idle timeout example to be commented"
        );
    }

    #[test]
    fn init_template_includes_reference_link_and_option_examples() {
        let rendered = generate_template(
            "demo-app",
            Some("server/index.mjs"),
            "demo-app.example.com",
            Some("bun"),
            None,
            &[],
            &[],
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
            None,
            &[],
            &[],
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
            None,
            &[],
            &[],
        );
        assert!(rendered.contains("[envs.production]\nroute = \"api.demo-app.com\""));
        assert!(!rendered.contains("[envs.production]\nroute = \"demo-app.example.com\""));
    }

    #[test]
    fn init_template_can_leave_preset_unset() {
        let rendered = generate_template(
            "demo-app",
            None,
            "demo-app.example.com",
            Some("node"),
            None,
            &[],
            &[],
        );
        assert!(rendered.contains("runtime = \"node\"\n# preset = \"my-node-preset\""));
    }

    #[test]
    fn init_template_writes_selected_build_adapter() {
        let rendered = generate_template(
            "demo-app",
            None,
            "demo-app.example.com",
            Some("bun"),
            None,
            &[],
            &[],
        );
        assert!(rendered.contains("runtime = \"bun\""));
    }

    #[test]
    fn init_template_writes_runtime_local_preset_reference() {
        let rendered = generate_template(
            "demo-app",
            None,
            "demo-app.example.com",
            Some("bun"),
            Some("tanstack-start"),
            &[],
            &[],
        );
        assert!(rendered.contains("preset = \"tanstack-start\""));
        assert!(!rendered.contains("preset = \"js/tanstack-start\""));
    }

    #[test]
    fn embedded_bun_preset_default_main_is_set() {
        assert_eq!(
            preset_default_main("bun", BuildAdapter::Bun, &[]),
            Some("src/index.ts".to_string())
        );
    }

    #[test]
    fn embedded_bun_tanstack_start_preset_default_main_is_set() {
        let presets = vec![FamilyPresetDefinition {
            name: "tanstack-start".to_string(),
            main: Some("dist/server/tako-entry.mjs".to_string()),
        }];
        assert_eq!(
            preset_default_main("tanstack-start", BuildAdapter::Bun, &presets),
            Some("dist/server/tako-entry.mjs".to_string())
        );
    }

    #[test]
    fn normalize_family_preset_names_excludes_base_and_deduplicates() {
        let names = normalize_family_preset_definitions(
            BuildAdapter::Bun,
            vec![
                FamilyPresetDefinition {
                    name: "".to_string(),
                    main: None,
                },
                FamilyPresetDefinition {
                    name: "bun".to_string(),
                    main: None,
                },
                FamilyPresetDefinition {
                    name: " tanstack-start ".to_string(),
                    main: Some("dist/server/tako-entry.mjs".to_string()),
                },
                FamilyPresetDefinition {
                    name: "tanstack-start".to_string(),
                    main: Some("dist/server/ignored.mjs".to_string()),
                },
                FamilyPresetDefinition {
                    name: "custom".to_string(),
                    main: None,
                },
            ],
        );
        assert_eq!(
            names,
            vec![
                FamilyPresetDefinition {
                    name: "tanstack-start".to_string(),
                    main: Some("dist/server/tako-entry.mjs".to_string()),
                },
                FamilyPresetDefinition {
                    name: "custom".to_string(),
                    main: None,
                },
            ]
        );
    }

    #[test]
    fn build_preset_selection_options_returns_none_when_no_family_presets_found() {
        let options = build_preset_selection_options(BuildAdapter::Bun, &[]);
        assert!(options.is_none());
    }

    #[test]
    fn build_preset_selection_options_includes_presets_and_custom_mode() {
        let options = build_preset_selection_options(
            BuildAdapter::Node,
            &["tanstack-start".to_string(), "next-start".to_string()],
        )
        .expect("options should be available");

        assert_eq!(options.len(), 3);
        assert_eq!(
            options[0],
            (
                "tanstack-start".to_string(),
                Some("tanstack-start".to_string())
            )
        );
        assert_eq!(
            options[1],
            ("next-start".to_string(), Some("next-start".to_string()))
        );
        assert_eq!(options[2], ("custom".to_string(), None));
    }

    #[test]
    fn resolve_adapter_uses_existing_config_runtime() {
        use crate::config::TakoToml;
        let existing = TakoToml {
            runtime: Some("deno".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_adapter(BuildAdapter::Node, Some(&existing)),
            BuildAdapter::Deno
        );
    }

    #[test]
    fn resolve_adapter_defaults_unknown_detection_to_bun() {
        assert_eq!(
            resolve_adapter(BuildAdapter::Unknown, None),
            BuildAdapter::Bun
        );
    }
}
