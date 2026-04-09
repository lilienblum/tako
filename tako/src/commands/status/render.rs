use super::remote::{display_server_version, sort_global_apps};
use super::time::format_deployed_at;
use super::{GlobalServerStatusResult, ServerStatusResult};
use crate::config::ServersToml;
use crate::output;
use tako_core::{AppState, InstanceState};

enum CardEntry {
    Field {
        label: String,
        value: String,
        color: Option<CardColor>,
    },
    Section {
        label: String,
        children: Vec<(String, String, Option<CardColor>)>,
    },
}

#[derive(Clone, Copy)]
pub(super) enum CardColor {
    Success,
    Warning,
    Error,
}

fn colorize(text: &str, color: Option<CardColor>) -> String {
    match color {
        Some(CardColor::Success) => output::theme_success(text),
        Some(CardColor::Warning) => output::theme_warning(text),
        Some(CardColor::Error) => output::theme_error(text),
        None => text.to_string(),
    }
}

pub(super) fn render_global_status(
    servers: &ServersToml,
    server_names: &[String],
    server_results: &mut std::collections::HashMap<String, GlobalServerStatusResult>,
) {
    struct Card {
        header: String,
        entries: Vec<CardEntry>,
    }

    let mut cards: Vec<Card> = Vec::new();
    let mut max_label = 0usize;

    for server_name in server_names {
        let Some(global) = server_results.remove(server_name.as_str()) else {
            continue;
        };
        let entry = servers.get(server_name.as_str());

        let mut entries: Vec<CardEntry> = Vec::new();
        let header = format!("Server {}", output::strong(server_name));

        if let Some(ref err) = global.error {
            entries.push(CardEntry::Field {
                label: "Error".into(),
                value: err.clone(),
                color: Some(CardColor::Error),
            });
            cards.push(Card { header, entries });
            continue;
        }

        let (status_label, status_color) = service_status_display(&global.service_status);
        entries.push(CardEntry::Field {
            label: "Status".into(),
            value: status_label,
            color: Some(status_color),
        });

        let is_offline = global.service_status != "active";

        if let Some(ref ver) = global.server_version {
            entries.push(CardEntry::Field {
                label: "Version".into(),
                value: display_server_version(ver),
                color: None,
            });
        }

        if let Some(ref uptime) = global.server_uptime {
            entries.push(CardEntry::Field {
                label: "Uptime".into(),
                value: uptime.clone(),
                color: None,
            });
        }

        if global.service_status != "upgrading"
            && let Some(ref uptime) = global.process_uptime
        {
            entries.push(CardEntry::Field {
                label: "Server uptime".into(),
                value: uptime.clone(),
                color: None,
            });
        }

        if !global.routes.is_empty() {
            let mut children: Vec<(String, String, Option<CardColor>)> = Vec::new();
            let mut last_app = String::new();
            for (app, pattern) in &global.routes {
                let label = if *app == last_app {
                    String::new()
                } else {
                    last_app.clone_from(app);
                    app.clone()
                };
                children.push((label, pattern.clone(), None));
            }
            entries.push(CardEntry::Section {
                label: "Routes".into(),
                children,
            });
        }

        if !global.apps.is_empty() && !is_offline {
            let mut apps = global.apps.clone();
            sort_global_apps(&mut apps);
            let mut children: Vec<(String, String, Option<CardColor>)> = Vec::new();
            for app in &apps {
                let (state, color) = app_state_summary(Some(&app.status));
                children.push((app.app_name.clone(), state, Some(color)));

                if app.env_name != "unknown" {
                    children.push(("  Environment".into(), app.env_name.clone(), None));
                }

                if let Some(ref app_status) = app.status.app_status {
                    let healthy = app_status
                        .instances
                        .iter()
                        .filter(|i| {
                            i.state == InstanceState::Healthy || i.state == InstanceState::Ready
                        })
                        .count();
                    let total = app_status.instances.len();
                    if total > 0 {
                        children.push((
                            "  Instances".into(),
                            format!("{healthy}/{total} healthy"),
                            None,
                        ));
                    }

                    if !app_status.version.is_empty() {
                        children.push(("  Release".into(), app_status.version.clone(), None));
                    }
                }

                if let Some(unix_secs) = app.status.deployed_at_unix_secs
                    && let Some(formatted) = format_deployed_at(unix_secs)
                {
                    children.push(("  Deployed at".into(), formatted, None));
                }
            }
            entries.push(CardEntry::Section {
                label: "Apps".into(),
                children,
            });
        }

        if let Some(desc) = entry
            .and_then(|e| e.description.as_deref())
            .filter(|d| !d.trim().is_empty())
        {
            entries.push(CardEntry::Field {
                label: "Description".into(),
                value: desc.to_string(),
                color: None,
            });
        }

        for entry in &entries {
            match entry {
                CardEntry::Field { label, .. } => {
                    max_label = max_label.max(label.len());
                }
                CardEntry::Section { label, children } => {
                    max_label = max_label.max(label.len());
                    for (child_label, _, _) in children {
                        max_label = max_label.max(child_label.len());
                    }
                }
            }
        }

        cards.push(Card { header, entries });
    }

    let indent = output::INDENT;
    for card in &cards {
        eprintln!("{}", card.header);
        if !card.entries.is_empty() {
            for entry in &card.entries {
                match entry {
                    CardEntry::Field {
                        label,
                        value,
                        color,
                    } => {
                        let colored_value = colorize(value, *color);
                        let padded = format!("{:<width$}", label, width = max_label);
                        eprintln!("{indent}{}  {colored_value}", output::theme_muted(&padded),);
                    }
                    CardEntry::Section { label, children } => {
                        eprintln!("{indent}{}", output::theme_muted(label));

                        for (ci, (child_label, child_value, child_color)) in
                            children.iter().enumerate()
                        {
                            let colored_value = colorize(child_value, *child_color);
                            if child_label.starts_with("  ") {
                                let is_first_sub = ci == 0 || !children[ci - 1].0.starts_with("  ");
                                let branch = if is_first_sub { "└" } else { " " };
                                let trimmed = child_label.trim_start();
                                let padded = format!(
                                    "{:<width$}",
                                    trimmed,
                                    width = max_label.saturating_sub(2)
                                );
                                eprintln!(
                                    "{indent}  {} {}  {colored_value}",
                                    output::theme_muted(branch),
                                    output::theme_muted(&padded),
                                );
                            } else {
                                let branch = if child_label.is_empty() { " " } else { "└" };
                                let padded = format!("{:<width$}", child_label, width = max_label);
                                eprintln!(
                                    "{indent}{} {}  {colored_value}",
                                    output::theme_muted(branch),
                                    output::theme_muted(&padded),
                                );
                            }
                        }
                    }
                }
            }
        }
        eprintln!();
    }
}

pub(super) fn service_status_display(status: &str) -> (String, CardColor) {
    match status {
        "active" => ("active".into(), CardColor::Success),
        "upgrading" => ("upgrading".into(), CardColor::Warning),
        "inactive" | "failed" => ("offline".into(), CardColor::Error),
        "unknown" => ("offline".into(), CardColor::Error),
        other => (other.to_string(), CardColor::Warning),
    }
}

pub(super) fn app_state_summary(status: Option<&ServerStatusResult>) -> (String, CardColor) {
    let Some(result) = status else {
        return ("unknown".into(), CardColor::Warning);
    };

    if let Some(app_status) = &result.app_status {
        let healthy = app_status
            .instances
            .iter()
            .filter(|i| i.state == InstanceState::Healthy || i.state == InstanceState::Ready)
            .count();
        let total = app_status.instances.len();

        return match app_status.state {
            AppState::Running => (format!("healthy {healthy}/{total}"), CardColor::Success),
            AppState::Idle => ("idle".into(), CardColor::Warning),
            AppState::Deploying => ("deploying".into(), CardColor::Warning),
            AppState::Stopped => ("stopped".into(), CardColor::Warning),
            AppState::Error => ("error".into(), CardColor::Error),
        };
    }

    if result.service_status == "active" {
        ("not deployed".into(), CardColor::Warning)
    } else {
        ("unavailable".into(), CardColor::Error)
    }
}
