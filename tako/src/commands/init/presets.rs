use crate::build::{
    BuildAdapter, PresetDefinition, PresetGroup, load_available_group_preset_definitions,
};
use crate::output;

pub(super) fn fetch_group_presets_for_adapter(
    adapter: BuildAdapter,
) -> std::io::Result<Vec<PresetDefinition>> {
    if !output::is_interactive() {
        return Ok(Vec::new());
    }

    let group = adapter.preset_group();
    if group == PresetGroup::Unknown {
        return Ok(Vec::new());
    }

    let runtime = tokio::runtime::Runtime::new().map_err(|e| {
        std::io::Error::other(format!("Failed to initialize preset fetch runtime: {e}"))
    })?;

    match runtime.block_on(load_available_group_preset_definitions(group)) {
        Ok(presets) => Ok(normalize_group_preset_definitions(adapter, presets)),
        Err(_) => Ok(Vec::new()),
    }
}

pub(super) fn normalize_group_preset_definitions(
    adapter: BuildAdapter,
    preset_definitions: Vec<PresetDefinition>,
) -> Vec<PresetDefinition> {
    let base = adapter.default_preset();
    let mut normalized = Vec::new();
    for preset in preset_definitions {
        let trimmed = preset.name.trim();
        if trimmed.is_empty() || trimmed == base {
            continue;
        }
        if normalized
            .iter()
            .any(|existing: &PresetDefinition| existing.name == trimmed)
        {
            continue;
        }
        normalized.push(PresetDefinition {
            name: trimmed.to_string(),
            main: preset.main,
        });
    }
    normalized
}

pub(super) fn build_preset_selection_options(
    _adapter: BuildAdapter,
    group_presets: &[String],
) -> Option<Vec<(String, Option<String>)>> {
    if group_presets.is_empty() {
        return None;
    }

    let mut options: Vec<(String, Option<String>)> = Vec::with_capacity(group_presets.len() + 1);
    for preset in group_presets {
        options.push((preset.clone(), Some(preset.clone())));
    }
    options.push(("custom".to_string(), None));

    Some(options)
}
