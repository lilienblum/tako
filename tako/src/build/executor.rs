//! Build executor - runs build commands and creates archives

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thiserror::Error;

/// Errors that can occur during build
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("Build command failed: {0}")]
    CommandFailed(String),

    #[error("Build command not found: {0}")]
    CommandNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Failed to create archive: {0}")]
    ArchiveError(String),

    #[error("Git error: {0}")]
    GitError(String),
}

/// Result of running a build command
#[derive(Debug)]
pub struct BuildResult {
    /// Whether the build succeeded
    pub success: bool,
    /// Combined stdout output
    pub stdout: String,
    /// Combined stderr output
    pub stderr: String,
    /// Exit code
    pub exit_code: Option<i32>,
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

    /// Run a build command
    pub fn run_build(&self, command: &str) -> Result<BuildResult, BuildError> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Err(BuildError::CommandFailed("Empty command".to_string()));
        }

        let program = parts[0];
        let args = &parts[1..];

        let output = Command::new(program)
            .args(args)
            .current_dir(&self.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    BuildError::CommandNotFound(program.to_string())
                } else {
                    BuildError::Io(e)
                }
            })?;

        Ok(BuildResult {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code(),
        })
    }

    /// Get the current git commit hash (short form)
    pub fn get_git_commit(&self) -> Result<String, BuildError> {
        let output = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&self.cwd)
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

    /// Create a deployment archive (.tar.gz)
    pub fn create_archive(
        &self,
        source_dir: &Path,
        output_path: &Path,
        exclude_patterns: &[&str],
    ) -> Result<u64, BuildError> {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        // Create parent directory if needed
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = std::fs::File::create(output_path)?;
        let encoder = GzEncoder::new(file, Compression::default());
        let mut archive = tar::Builder::new(encoder);

        // Default exclusions
        let default_excludes = [
            ".git",
            "node_modules",
            ".tako",
            "target",
            ".env",
            ".env.local",
            "*.log",
        ];

        // Walk directory and add files
        self.add_dir_to_archive(
            &mut archive,
            source_dir,
            source_dir,
            &default_excludes,
            exclude_patterns,
        )?;

        let encoder = archive
            .into_inner()
            .map_err(|e| BuildError::ArchiveError(format!("Failed to finish archive: {}", e)))?;

        encoder
            .finish()
            .map_err(|e| BuildError::ArchiveError(format!("Failed to compress: {}", e)))?;

        // Return file size
        let metadata = std::fs::metadata(output_path)?;
        Ok(metadata.len())
    }

    fn add_dir_to_archive<W: Write>(
        &self,
        archive: &mut tar::Builder<W>,
        base_dir: &Path,
        current_dir: &Path,
        default_excludes: &[&str],
        custom_excludes: &[&str],
    ) -> Result<(), BuildError> {
        let entries = std::fs::read_dir(current_dir)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_name = path.file_name().unwrap().to_string_lossy();

            // Check exclusions
            let should_exclude = default_excludes.iter().any(|p| {
                if let Some(suffix) = p.strip_prefix('*') {
                    file_name.ends_with(suffix)
                } else {
                    file_name == *p
                }
            }) || custom_excludes.iter().any(|p| {
                if let Some(suffix) = p.strip_prefix('*') {
                    file_name.ends_with(suffix)
                } else {
                    file_name == *p
                }
            });

            if should_exclude {
                continue;
            }

            let relative_path = path.strip_prefix(base_dir).unwrap();

            if path.is_dir() {
                self.add_dir_to_archive(
                    archive,
                    base_dir,
                    &path,
                    default_excludes,
                    custom_excludes,
                )?;
            } else if path.is_file() {
                archive
                    .append_path_with_name(&path, relative_path)
                    .map_err(|e| {
                        BuildError::ArchiveError(format!("Failed to add {}: {}", path.display(), e))
                    })?;
            }
        }

        Ok(())
    }

    /// Extract an archive to a directory
    pub fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<(), BuildError> {
        use flate2::read::GzDecoder;

        std::fs::create_dir_all(dest_dir)?;

        let file = std::fs::File::open(archive_path)?;
        let decoder = GzDecoder::new(file);
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

/// Compute SHA256 hash of directory contents (for dirty detection)
pub fn compute_dir_hash(dir: &Path, exclude_patterns: &[&str]) -> Result<String, BuildError> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    let mut paths: Vec<PathBuf> = Vec::new();

    // Collect all file paths
    collect_files(dir, &mut paths, exclude_patterns)?;

    // Sort for deterministic ordering
    paths.sort();

    // Hash each file's path and content
    for path in paths {
        let relative = path.strip_prefix(dir).unwrap();
        hasher.update(relative.to_string_lossy().as_bytes());

        let mut file = std::fs::File::open(&path)?;
        let mut buffer = [0u8; 8192];
        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }
    }

    let result = hasher.finalize();
    Ok(hex::encode(result))
}

fn short_hash(s: &str) -> &str {
    &s[..8.min(s.len())]
}

fn collect_files(
    dir: &Path,
    paths: &mut Vec<PathBuf>,
    exclude_patterns: &[&str],
) -> Result<(), BuildError> {
    let default_excludes = [".git", "node_modules", ".tako", "target"];

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = path.file_name().unwrap().to_string_lossy();

        // Check exclusions
        let should_exclude = default_excludes.iter().any(|p| file_name == *p)
            || exclude_patterns.iter().any(|p| {
                if let Some(suffix) = p.strip_prefix('*') {
                    file_name.ends_with(suffix)
                } else {
                    file_name == *p
                }
            });

        if should_exclude {
            continue;
        }

        if path.is_dir() {
            collect_files(&path, paths, exclude_patterns)?;
        } else if path.is_file() {
            paths.push(path);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_run_build_echo() {
        let temp = TempDir::new().unwrap();
        let executor = BuildExecutor::new(temp.path());

        let result = executor.run_build("echo hello").unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("hello"));
    }

    #[test]
    fn test_run_build_failure() {
        let temp = TempDir::new().unwrap();
        let executor = BuildExecutor::new(temp.path());

        let result = executor.run_build("false").unwrap();
        assert!(!result.success);
    }

    #[test]
    fn test_run_build_not_found() {
        let temp = TempDir::new().unwrap();
        let executor = BuildExecutor::new(temp.path());

        let result = executor.run_build("nonexistent_command_12345");
        assert!(matches!(result, Err(BuildError::CommandNotFound(_))));
    }

    #[test]
    fn test_create_and_extract_archive() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let archive_path = temp.path().join("test.tar.gz");
        let dest = temp.path().join("dest");

        // Create source files
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file1.txt"), "content1").unwrap();
        fs::create_dir_all(source.join("subdir")).unwrap();
        fs::write(source.join("subdir/file2.txt"), "content2").unwrap();

        // Create archive
        let executor = BuildExecutor::new(&source);
        let size = executor
            .create_archive(&source, &archive_path, &[])
            .unwrap();
        assert!(size > 0);
        assert!(archive_path.exists());

        // Extract archive
        BuildExecutor::extract_archive(&archive_path, &dest).unwrap();
        assert!(dest.join("file1.txt").exists());
        assert!(dest.join("subdir/file2.txt").exists());

        // Verify contents
        assert_eq!(
            fs::read_to_string(dest.join("file1.txt")).unwrap(),
            "content1"
        );
        assert_eq!(
            fs::read_to_string(dest.join("subdir/file2.txt")).unwrap(),
            "content2"
        );
    }

    #[test]
    fn test_archive_excludes_node_modules() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let archive_path = temp.path().join("test.tar.gz");
        let dest = temp.path().join("dest");

        // Create source with node_modules
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("index.js"), "console.log('hello')").unwrap();
        fs::create_dir_all(source.join("node_modules/dep")).unwrap();
        fs::write(source.join("node_modules/dep/index.js"), "module").unwrap();

        // Create archive
        let executor = BuildExecutor::new(&source);
        executor
            .create_archive(&source, &archive_path, &[])
            .unwrap();

        // Extract and verify node_modules excluded
        BuildExecutor::extract_archive(&archive_path, &dest).unwrap();
        assert!(dest.join("index.js").exists());
        assert!(!dest.join("node_modules").exists());
    }

    #[test]
    fn test_compute_file_hash() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let hash = compute_file_hash(&file_path).unwrap();
        // SHA256 of "hello world"
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_compute_dir_hash_deterministic() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("project");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "aaa").unwrap();
        fs::write(dir.join("b.txt"), "bbb").unwrap();

        let hash1 = compute_dir_hash(&dir, &[]).unwrap();
        let hash2 = compute_dir_hash(&dir, &[]).unwrap();
        assert_eq!(hash1, hash2);

        // Modify a file
        fs::write(dir.join("a.txt"), "changed").unwrap();
        let hash3 = compute_dir_hash(&dir, &[]).unwrap();
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_generate_version_falls_back_when_git_commit_missing() {
        let temp = TempDir::new().unwrap();
        let executor = BuildExecutor::new(temp.path());

        let version = executor.generate_version(Some("abcdef123456")).unwrap();
        assert_eq!(version, "nogit_abcdef12");
    }

    #[test]
    fn test_generate_version_falls_back_with_timestamp_when_no_hash() {
        let temp = TempDir::new().unwrap();
        let executor = BuildExecutor::new(temp.path());

        let version = executor.generate_version(None).unwrap();
        assert!(version.starts_with("nogit_"));
        assert!(version.len() > "nogit_".len());
    }
}
