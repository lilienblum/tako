//! Build source hashes and deployment versions.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Errors that can occur during build
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Failed to create archive: {0}")]
    ArchiveError(String),

    #[error("Git error: {0}")]
    GitError(String),
}

/// Build executor
pub struct BuildExecutor {
    /// Working directory
    cwd: PathBuf,
}

impl BuildExecutor {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }

    /// Get the current git commit hash (short form)
    pub fn get_git_commit(&self) -> Result<String, BuildError> {
        let output = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&self.cwd)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .map_err(|e| BuildError::GitError(e.to_string()))?;

        if !output.status.success() {
            return Err(BuildError::GitError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Check if git working tree is dirty (has uncommitted changes)
    pub fn is_git_dirty(&self) -> Result<bool, BuildError> {
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.cwd)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .map_err(|e| BuildError::GitError(e.to_string()))?;

        if !output.status.success() {
            return Err(BuildError::GitError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        // If output is non-empty, there are uncommitted changes
        Ok(!output.stdout.is_empty())
    }

    /// Generate version string for deployment
    /// Format: {commit} or {commit}_{content_hash} if dirty
    pub fn generate_version(&self, content_hash: Option<&str>) -> Result<String, BuildError> {
        let commit = match self.get_git_commit() {
            Ok(commit) => commit,
            Err(_) => {
                // Fallback for directories without commits/repos.
                let suffix = if let Some(hash) = content_hash {
                    short_hash(hash).to_string()
                } else {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs().to_string())
                        .unwrap_or_else(|_| "0".to_string())
                };
                return Ok(format!("nogit_{}", suffix));
            }
        };
        let dirty = self.is_git_dirty()?;

        if dirty {
            // Include content hash to differentiate dirty builds
            let hash = content_hash.unwrap_or("dirty");
            Ok(format!("{}_{}", commit, short_hash(hash)))
        } else {
            Ok(commit)
        }
    }

    /// True when generate_version would embed a content hash (no git commit, or dirty tree).
    pub fn version_needs_content_hash(&self) -> Result<bool, BuildError> {
        if self.get_git_commit().is_err() {
            return Ok(true);
        }
        self.is_git_dirty()
    }

    /// Compute SHA256 over source paths and contents, respecting gitignore and forced exclusions.
    pub fn compute_source_hash(&self, source_root: &Path) -> Result<String, BuildError> {
        use sha2::{Digest, Sha256};

        let files = collect_source_archive_files(source_root)?;
        let mut hasher = Sha256::new();

        for (full_path, relative_path) in files {
            hasher.update(relative_path.to_string_lossy().as_bytes());
            let metadata = std::fs::symlink_metadata(&full_path)?;
            if metadata.file_type().is_symlink() {
                // Hash the link target without following directory links.
                let target = std::fs::read_link(&full_path)?;
                hasher.update(b"symlink:");
                hasher.update(target.to_string_lossy().as_bytes());
            } else {
                let mut file = std::fs::File::open(&full_path)?;
                let mut buffer = [0u8; 8192];
                loop {
                    let bytes_read = file.read(&mut buffer)?;
                    if bytes_read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..bytes_read]);
                }
            }
        }

        Ok(hex::encode(hasher.finalize()))
    }

    /// Extract an archive to a directory
    #[cfg(test)]
    pub fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), BuildError> {
        std::fs::create_dir_all(dest_dir)?;

        let file = std::fs::File::open(archive_path)?;
        let decoder = zstd::stream::read::Decoder::new(file).map_err(|e| {
            BuildError::ArchiveError(format!("Failed to initialize zstd decoder: {}", e))
        })?;
        let mut archive = tar::Archive::new(decoder);

        archive
            .unpack(dest_dir)
            .map_err(|e| BuildError::ArchiveError(format!("Failed to extract: {}", e)))?;

        Ok(())
    }
}

/// Compute SHA256 hash of file contents
pub fn compute_file_hash(path: &Path) -> Result<String, BuildError> {
    use sha2::{Digest, Sha256};

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let result = hasher.finalize();
    Ok(hex::encode(result))
}

fn short_hash(s: &str) -> &str {
    &s[..8.min(s.len())]
}

fn should_force_exclude_from_source_archive(relative_path: &Path) -> bool {
    for component in relative_path.components() {
        if let Component::Normal(name) = component {
            match name.to_str() {
                Some(".git") | Some(".tako") | Some("node_modules") | Some("target") => {
                    return true;
                }
                Some(name) if name.starts_with(".env") => return true,
                _ => {}
            }
        }
    }
    false
}

fn collect_source_archive_files(source_root: &Path) -> Result<Vec<(PathBuf, PathBuf)>, BuildError> {
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut walker = ignore::WalkBuilder::new(source_root);
    walker
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false);

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

        if should_force_exclude_from_source_archive(relative_path) {
            continue;
        }

        files.push((path.to_path_buf(), relative_path.to_path_buf()));
    }

    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
}

#[cfg(test)]
mod tests;
