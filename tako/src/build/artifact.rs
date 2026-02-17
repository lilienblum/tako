use std::path::{Component, Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::BuildError;

pub fn create_filtered_archive(
    source_root: &Path,
    output_path: &Path,
    include_patterns: &[String],
    exclude_patterns: &[String],
) -> Result<u64, BuildError> {
    create_filtered_archive_with_prefix(
        source_root,
        output_path,
        include_patterns,
        exclude_patterns,
        None,
    )
}

pub fn create_filtered_archive_with_prefix(
    source_root: &Path,
    output_path: &Path,
    include_patterns: &[String],
    exclude_patterns: &[String],
    archive_prefix: Option<&Path>,
) -> Result<u64, BuildError> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let includes = compile_patterns(source_root, include_patterns, "include")?;
    let excludes = compile_patterns(source_root, exclude_patterns, "exclude")?;

    let mut files = collect_files_for_archive(source_root, &includes, &excludes)?;
    files.sort_by(|a, b| a.1.cmp(&b.1));

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = std::fs::File::create(output_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = tar::Builder::new(encoder);
    archive.follow_symlinks(false);

    for (full_path, relative_path) in files {
        let archive_path = match archive_prefix {
            Some(prefix) => prefix.join(&relative_path),
            None => relative_path,
        };
        archive
            .append_path_with_name(&full_path, &archive_path)
            .map_err(|e| {
                BuildError::ArchiveError(format!("Failed to add {}: {}", full_path.display(), e))
            })?;
    }

    let encoder = archive
        .into_inner()
        .map_err(|e| BuildError::ArchiveError(format!("Failed to finish archive: {}", e)))?;

    encoder
        .finish()
        .map_err(|e| BuildError::ArchiveError(format!("Failed to compress: {}", e)))?;

    let metadata = std::fs::metadata(output_path)?;
    Ok(metadata.len())
}

fn collect_files_for_archive(
    source_root: &Path,
    includes: &Option<Gitignore>,
    excludes: &Option<Gitignore>,
) -> Result<Vec<(PathBuf, PathBuf)>, BuildError> {
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut walker = ignore::WalkBuilder::new(source_root);
    walker
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false);

    for entry in walker.build() {
        let entry = entry.map_err(|e| BuildError::ArchiveError(e.to_string()))?;
        let file_type = match entry.file_type() {
            Some(file_type) => file_type,
            None => continue,
        };
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }

        let path = entry.path();
        let relative_path = path.strip_prefix(source_root).map_err(|e| {
            BuildError::ArchiveError(format!(
                "Failed to compute relative path for {}: {}",
                path.display(),
                e
            ))
        })?;

        if should_force_exclude(relative_path) {
            continue;
        }

        if let Some(include_matcher) = includes
            && !include_matcher
                .matched_path_or_any_parents(relative_path, false)
                .is_ignore()
        {
            continue;
        }
        if let Some(exclude_matcher) = excludes
            && exclude_matcher
                .matched_path_or_any_parents(relative_path, false)
                .is_ignore()
        {
            continue;
        }

        files.push((path.to_path_buf(), relative_path.to_path_buf()));
    }

    Ok(files)
}

fn compile_patterns(
    source_root: &Path,
    patterns: &[String],
    kind: &str,
) -> Result<Option<Gitignore>, BuildError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GitignoreBuilder::new(source_root);
    for pattern in patterns {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return Err(BuildError::ArchiveError(format!(
                "artifact {} patterns cannot contain empty entries",
                kind
            )));
        }
        builder.add_line(None, trimmed).map_err(|e| {
            BuildError::ArchiveError(format!("invalid {} glob '{}': {}", kind, trimmed, e))
        })?;
    }
    let gitignore = builder.build().map_err(|e| {
        BuildError::ArchiveError(format!("failed to build {} matcher: {}", kind, e))
    })?;
    Ok(Some(gitignore))
}

fn should_force_exclude(relative_path: &Path) -> bool {
    for component in relative_path.components() {
        if let Component::Normal(name) = component {
            match name.to_str() {
                Some(".git") | Some(".tako") => return true,
                Some(name) if name.starts_with(".env") => return true,
                _ => {}
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::BuildExecutor;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn filtered_archive_uses_include_and_exclude_globs() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let archive = temp.path().join("out.tar.gz");
        let dest = temp.path().join("dest");

        fs::create_dir_all(source.join("dist/client")).unwrap();
        fs::create_dir_all(source.join("dist/server")).unwrap();
        fs::write(source.join("dist/client/index.js"), "client").unwrap();
        fs::write(source.join("dist/client/index.js.map"), "map").unwrap();
        fs::write(source.join("dist/server/main.js"), "server").unwrap();
        fs::write(source.join("README.md"), "readme").unwrap();

        create_filtered_archive(
            &source,
            &archive,
            &[String::from("dist/**")],
            &[String::from("**/*.map")],
        )
        .unwrap();

        BuildExecutor::extract_archive(&archive, &dest).unwrap();
        assert!(dest.join("dist/client/index.js").exists());
        assert!(dest.join("dist/server/main.js").exists());
        assert!(!dest.join("dist/client/index.js.map").exists());
        assert!(!dest.join("README.md").exists());
    }

    #[test]
    fn filtered_archive_can_prefix_paths() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let archive = temp.path().join("out.tar.gz");
        let dest = temp.path().join("dest");

        fs::create_dir_all(source.join("dist/client")).unwrap();
        fs::write(source.join("dist/client/index.js"), "client").unwrap();

        create_filtered_archive_with_prefix(
            &source,
            &archive,
            &[String::from("dist/**")],
            &[],
            Some(Path::new("apps/web")),
        )
        .unwrap();

        BuildExecutor::extract_archive(&archive, &dest).unwrap();
        assert!(dest.join("apps/web/dist/client/index.js").exists());
        assert!(!dest.join("dist/client/index.js").exists());
    }

    #[test]
    fn filtered_archive_force_excludes_sensitive_paths() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let archive = temp.path().join("out.tar.gz");
        let dest = temp.path().join("dest");

        fs::create_dir_all(source.join(".git")).unwrap();
        fs::create_dir_all(source.join(".tako/cache")).unwrap();
        fs::create_dir_all(source.join("dist")).unwrap();
        fs::write(source.join(".git/config"), "git").unwrap();
        fs::write(source.join(".tako/cache/data"), "cache").unwrap();
        fs::write(source.join(".env.production"), "secret").unwrap();
        fs::write(source.join("dist/index.js"), "ok").unwrap();

        create_filtered_archive(&source, &archive, &[String::from("**/*")], &[]).unwrap();

        BuildExecutor::extract_archive(&archive, &dest).unwrap();
        assert!(dest.join("dist/index.js").exists());
        assert!(!dest.join(".git/config").exists());
        assert!(!dest.join(".tako/cache/data").exists());
        assert!(!dest.join(".env.production").exists());
    }

    #[test]
    fn filtered_archive_keeps_node_modules_when_not_excluded() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let archive = temp.path().join("out.tar.gz");
        let dest = temp.path().join("dest");

        fs::create_dir_all(source.join("node_modules/tako.sh/src")).unwrap();
        fs::write(
            source.join("node_modules/tako.sh/src/wrapper.ts"),
            "export {}",
        )
        .unwrap();

        create_filtered_archive(&source, &archive, &[String::from("**/*")], &[]).unwrap();

        BuildExecutor::extract_archive(&archive, &dest).unwrap();
        assert!(dest.join("node_modules/tako.sh/src/wrapper.ts").exists());
    }

    #[cfg(unix)]
    #[test]
    fn filtered_archive_preserves_symlinks() {
        use std::os::unix::fs as unix_fs;

        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let archive = temp.path().join("out.tar.gz");
        let dest = temp.path().join("dest");

        fs::create_dir_all(source.join("sdk")).unwrap();
        fs::create_dir_all(source.join("app/node_modules")).unwrap();
        fs::write(source.join("sdk/index.js"), "ok").unwrap();
        unix_fs::symlink("../../sdk", source.join("app/node_modules/tako.sh")).unwrap();

        create_filtered_archive(&source, &archive, &[String::from("**/*")], &[]).unwrap();

        BuildExecutor::extract_archive(&archive, &dest).unwrap();
        let metadata = fs::symlink_metadata(dest.join("app/node_modules/tako.sh")).unwrap();
        assert!(metadata.file_type().is_symlink());
    }
}
