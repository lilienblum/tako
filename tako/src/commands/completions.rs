use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::CommandFactory;
use clap_complete::{generate, shells};

use crate::cli::Cli;
use crate::output;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTarget {
    pub shell: &'static str,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionPlan {
    pub targets: Vec<CompletionTarget>,
    pub zsh_needs_fpath_hint: bool,
}

pub fn run(yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("could not determine home directory")?;
    let available = detected_shells();
    if available.is_empty() {
        return Err("no supported shells found (bash, zsh, fish)".into());
    }

    let zsh_fpath = zsh_fpath_dirs();
    let plan = plan(&home, &available, &zsh_fpath);
    if plan.targets.is_empty() && !plan.zsh_needs_fpath_hint {
        return Err("no completion directories available".into());
    }

    if !yes {
        if !output::is_interactive() {
            return Err("re-run with --yes to install completions".into());
        }
        if !plan.targets.is_empty() {
            let names = join_and(plan.targets.iter().map(|t| t.shell));
            let description = plan
                .targets
                .iter()
                .map(|t| t.path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let confirmed = output::confirm_with_description(
                &format!("Install completions for {names}?"),
                Some(&description),
                true,
            )?;
            if !confirmed {
                output::operation_cancelled();
                return Ok(());
            }
        }
    }

    if output::is_dry_run() {
        for target in &plan.targets {
            output::dry_run_skip(&format!(
                "Write {} completions to {}",
                target.shell,
                target.path.display()
            ));
        }
        hint_zsh_fpath(&plan);
        return Ok(());
    }

    for target in &plan.targets {
        write_completion(target)?;
    }

    if !plan.targets.is_empty() {
        let names = join_and(plan.targets.iter().map(|t| t.shell));
        output::success(&format!("Installed completions for {names}"));
    }
    hint_zsh_fpath(&plan);
    Ok(())
}

pub fn existing_files() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut paths = vec![bash_path(&home), fish_path(&home)];
    for dir in zsh_fpath_dirs() {
        paths.push(dir.join("_tako"));
    }
    paths.into_iter().filter(|p| p.is_file()).collect()
}

pub fn plan(home: &Path, available: &[&str], zsh_fpath: &[PathBuf]) -> CompletionPlan {
    let mut targets = Vec::new();
    let mut zsh_needs_fpath_hint = false;

    if available.contains(&"bash") {
        targets.push(CompletionTarget {
            shell: "bash",
            path: bash_path(home),
        });
    }
    if available.contains(&"zsh") {
        if let Some(dir) = zsh_fpath.iter().find(|dir| is_usable_dir(dir)) {
            targets.push(CompletionTarget {
                shell: "zsh",
                path: dir.join("_tako"),
            });
        } else {
            zsh_needs_fpath_hint = true;
        }
    }
    if available.contains(&"fish") {
        targets.push(CompletionTarget {
            shell: "fish",
            path: fish_path(home),
        });
    }

    CompletionPlan {
        targets,
        zsh_needs_fpath_hint,
    }
}

fn write_completion(target: &CompletionTarget) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = target.path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    match target.shell {
        "bash" => generate(shells::Bash, &mut cmd, "tako", &mut buf),
        "zsh" => generate(shells::Zsh, &mut cmd, "tako", &mut buf),
        "fish" => generate(shells::Fish, &mut cmd, "tako", &mut buf),
        other => return Err(format!("unsupported shell {other}").into()),
    }
    fs::write(&target.path, buf)?;
    Ok(())
}

fn hint_zsh_fpath(plan: &CompletionPlan) {
    if plan.zsh_needs_fpath_hint {
        output::hint("zsh completions were not installed (no writable directory on fpath).");
        output::hint("Add a writable directory to fpath, then run tako completions again.");
    }
}

fn detected_shells() -> Vec<&'static str> {
    ["bash", "zsh", "fish"]
        .into_iter()
        .filter(|name| binary_on_path(name))
        .collect()
}

fn binary_on_path(name: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

fn zsh_fpath_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for prefix in ["/opt/homebrew", "/usr/local"] {
        let dir = PathBuf::from(prefix).join("share/zsh/site-functions");
        if dir.is_dir() {
            dirs.push(dir);
        }
    }
    let output = Command::new("zsh")
        .args(["-f", "-c", "print -l -- $fpath"])
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let path = PathBuf::from(line);
            if !dirs.contains(&path) {
                dirs.push(path);
            }
        }
    }
    dirs
}

fn bash_path(home: &Path) -> PathBuf {
    data_home(home).join("bash-completion/completions/tako")
}

fn fish_path(home: &Path) -> PathBuf {
    config_home(home).join("fish/completions/tako.fish")
}

fn data_home(home: &Path) -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
}

fn config_home(home: &Path) -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
}

fn is_usable_dir(path: &Path) -> bool {
    if path.is_dir() {
        return probe_writable(path);
    }
    if let Some(parent) = path.parent()
        && parent.is_dir()
    {
        return probe_writable(parent);
    }
    false
}

fn probe_writable(dir: &Path) -> bool {
    let probe = dir.join(".tako-completions-write-test");
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(err) if err.kind() == ErrorKind::PermissionDenied => false,
        Err(_) => false,
    }
}

fn join_and<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let names: Vec<_> = names.collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_string(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn plan_skips_fish_when_missing() {
        let home = TempDir::new().unwrap();
        let plan = plan(home.path(), &["bash", "zsh"], &[]);
        assert_eq!(
            plan.targets.iter().map(|t| t.shell).collect::<Vec<_>>(),
            ["bash"]
        );
        assert!(plan.zsh_needs_fpath_hint);
    }

    #[test]
    fn plan_writes_zsh_when_fpath_is_writable() {
        let home = TempDir::new().unwrap();
        let fpath = home.path().join("zsh-site-functions");
        fs::create_dir_all(&fpath).unwrap();
        let plan = plan(home.path(), &["zsh"], std::slice::from_ref(&fpath));
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].path, fpath.join("_tako"));
        assert!(!plan.zsh_needs_fpath_hint);
    }

    #[test]
    fn plan_includes_fish_when_present() {
        let home = TempDir::new().unwrap();
        let plan = plan(home.path(), &["fish"], &[]);
        assert_eq!(plan.targets[0].path, fish_path(home.path()));
    }

    #[test]
    fn generated_bash_script_mentions_tako() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(shells::Bash, &mut cmd, "tako", &mut buf);
        let script = String::from_utf8(buf).unwrap();
        assert!(script.contains("tako"));
    }

    #[test]
    fn join_and_formats_two_and_three_names() {
        assert_eq!(join_and(["bash", "zsh"].into_iter()), "bash and zsh");
        assert_eq!(
            join_and(["bash", "zsh", "fish"].into_iter()),
            "bash, zsh and fish"
        );
    }
}
