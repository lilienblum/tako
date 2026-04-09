use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn ensure_project_gitignore_tracks_secrets(project_dir: &Path) -> std::io::Result<()> {
    let gitignore_root =
        find_git_repo_root(project_dir).unwrap_or_else(|| project_dir.to_path_buf());
    let gitignore_path = gitignore_root.join(".gitignore");
    let rules = [
        "# tako: ignore runtime artifacts, keep secrets".to_string(),
        "**/.tako/*".to_string(),
        "!**/.tako/secrets.json".to_string(),
    ];

    let mut content = if gitignore_path.exists() {
        fs::read_to_string(&gitignore_path)?
    } else {
        String::new()
    };
    let mut existing_lines = content
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let mut changed = false;

    for rule in rules {
        if existing_lines.insert(rule.clone()) {
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&rule);
            content.push('\n');
            changed = true;
        }
    }

    if changed {
        fs::write(gitignore_path, content)?;
    }

    Ok(())
}

pub(super) fn display_config_path_for_prompt(config_path: &Path, cwd: &Path) -> String {
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());

    config_path
        .strip_prefix(&canonical_cwd)
        .ok()
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| config_path.display().to_string())
}

pub(super) fn find_git_repo_root(project_dir: &Path) -> Option<PathBuf> {
    project_dir
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}
