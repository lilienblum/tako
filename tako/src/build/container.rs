use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use super::BuildPresetTarget;

const BUN_INSTALL_CACHE_PATH: &str = "/var/cache/tako/bun/install/cache";
const PROTO_HOME_PATH: &str = "/var/cache/tako/proto";
const CACHE_VOLUME_PREFIX: &str = "tako-build-cache";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContainerCacheMount {
    volume_name: String,
    container_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BuildStageCommand {
    pub name: Option<String>,
    pub working_dir: Option<String>,
    pub install: Option<String>,
    pub run: String,
}

pub fn run_container_build(
    workspace_dir: &Path,
    app_subdir: &str,
    target_label: &str,
    runtime_tool: &str,
    target: &BuildPresetTarget,
    stages: &[BuildStageCommand],
) -> Result<(), String> {
    let image = target
        .builder_image
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or(default_builder_image_for_target_label(target_label)?.to_string());
    let platform = docker_platform_for_target_label(target_label)?;
    let script = build_container_script(
        app_subdir,
        runtime_tool,
        target.install.as_deref(),
        target.build.as_deref(),
        stages,
    )?;
    let cache_mounts = dependency_cache_mounts(target_label, runtime_tool, &image);
    let workspace = workspace_dir.canonicalize().map_err(|e| {
        format!(
            "Failed to canonicalize workspace {}: {e}",
            workspace_dir.display()
        )
    })?;
    let create_args = build_docker_create_args(platform, &image, &script, &cache_mounts);
    let create_output = Command::new("docker")
        .args(&create_args)
        .output()
        .map_err(|e| format!("Failed to start docker for local build: {e}"))?;
    if !create_output.status.success() {
        let stderr = String::from_utf8_lossy(&create_output.stderr)
            .trim()
            .to_string();
        let stdout = String::from_utf8_lossy(&create_output.stdout)
            .trim()
            .to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!(
            "Failed to create build container for {target_label}: {detail}"
        ));
    }
    let container_id = String::from_utf8_lossy(&create_output.stdout)
        .trim()
        .to_string();
    if container_id.is_empty() {
        return Err(format!(
            "Failed to create build container for {target_label}: missing container id"
        ));
    }

    let result =
        run_container_build_with_existing_container(&container_id, &workspace, target_label);

    // Best-effort cleanup.
    let _ = Command::new("docker")
        .args(["rm", "-f", &container_id])
        .output();

    result
}

fn run_container_build_with_existing_container(
    container_id: &str,
    workspace_dir: &Path,
    target_label: &str,
) -> Result<(), String> {
    let workspace_copy_in = format!("{}:{}", container_id, "/workspace");
    let copy_in_output = Command::new("docker")
        .args([
            "cp",
            &format!("{}/.", workspace_dir.display()),
            &workspace_copy_in,
        ])
        .output()
        .map_err(|e| format!("Failed to copy workspace into build container: {e}"))?;
    if !copy_in_output.status.success() {
        let stderr = String::from_utf8_lossy(&copy_in_output.stderr)
            .trim()
            .to_string();
        return Err(format!(
            "Failed to copy workspace into build container for {target_label}: {stderr}"
        ));
    }

    let start_output = Command::new("docker")
        .args(["start", "-a", container_id])
        .output()
        .map_err(|e| format!("Failed to run build container: {e}"))?;
    if !start_output.status.success() {
        let stderr = String::from_utf8_lossy(&start_output.stderr)
            .trim()
            .to_string();
        let stdout = String::from_utf8_lossy(&start_output.stdout)
            .trim()
            .to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!(
            "Container build failed for {target_label}: {detail}"
        ));
    }

    let copy_out_output = Command::new("docker")
        .args([
            "cp",
            &format!("{}:{}", container_id, "/workspace/."),
            &workspace_dir.display().to_string(),
        ])
        .output()
        .map_err(|e| format!("Failed to copy build output from container: {e}"))?;
    if !copy_out_output.status.success() {
        let stderr = String::from_utf8_lossy(&copy_out_output.stderr)
            .trim()
            .to_string();
        return Err(format!(
            "Failed to copy build output from container for {target_label}: {stderr}"
        ));
    }

    Ok(())
}

fn build_docker_create_args(
    platform: &str,
    image: &str,
    script: &str,
    cache_mounts: &[ContainerCacheMount],
) -> Vec<String> {
    let mut args = vec![
        "create".to_string(),
        "--platform".to_string(),
        platform.to_string(),
        "-w".to_string(),
        "/workspace".to_string(),
    ];

    for mount in cache_mounts {
        args.push("--mount".to_string());
        args.push(format!(
            "type=volume,src={},dst={}",
            mount.volume_name, mount.container_path
        ));
    }

    args.push(image.to_string());
    args.push("sh".to_string());
    args.push("-lc".to_string());
    args.push(script.to_string());
    args
}

fn build_container_script(
    app_subdir: &str,
    runtime_tool: &str,
    install_command: Option<&str>,
    build_command: Option<&str>,
    stages: &[BuildStageCommand],
) -> Result<String, String> {
    let (app_subdir_value, app_dir) = resolve_app_dir(app_subdir)?;
    let has_preset_commands = install_command
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some()
        || build_command
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some();
    let mut lines = vec![
        "set -eu".to_string(),
        "(set -o pipefail >/dev/null 2>&1) || true".to_string(),
        format!(
            "export TAKO_APP_SUBDIR={}",
            shell_single_quote(&app_subdir_value)
        ),
        format!("export TAKO_APP_DIR={}", shell_single_quote(&app_dir)),
        format!("export PROTO_HOME={}", shell_single_quote(PROTO_HOME_PATH)),
        format!(
            "export PATH=\"$PROTO_HOME/bin:$PROTO_HOME/shims:$PATH\""
        ),
        format!(
            "export BUN_INSTALL_CACHE_DIR={}",
            shell_single_quote(BUN_INSTALL_CACHE_PATH)
        ),
        "if command -v apk >/dev/null 2>&1; then apk add --no-cache bash ca-certificates curl git gzip unzip xz; elif command -v apt-get >/dev/null 2>&1; then export DEBIAN_FRONTEND=noninteractive; apt-get update; apt-get install -y --no-install-recommends bash ca-certificates curl git gzip unzip xz-utils; rm -rf /var/lib/apt/lists/*; else echo \"Unsupported builder image: expected apk or apt-get\" >&2; exit 1; fi".to_string(),
        "if [ ! -x \"$PROTO_HOME/bin/proto\" ]; then installer=\"$(mktemp)\"; curl -fsSL https://moonrepo.dev/install/proto.sh -o \"$installer\"; chmod +x \"$installer\"; PROTO_HOME=\"$PROTO_HOME\" bash \"$installer\" --yes --no-profile; rm -f \"$installer\"; fi".to_string(),
        "if ! command -v proto >/dev/null 2>&1; then echo \"Failed to install proto in build container\" >&2; exit 1; fi".to_string(),
        format!(
            "if [ ! -f /workspace/.prototools ] && [ -f {} ]; then cp {} /workspace/.prototools; fi",
            shell_single_quote(&format!("{app_dir}/.prototools")),
            shell_single_quote(&format!("{app_dir}/.prototools"))
        ),
        format!(
            "if [ ! -f /workspace/.prototools ]; then printf '%s = \"latest\"\\n' {} > /workspace/.prototools; fi",
            shell_single_quote(runtime_tool)
        ),
        "cd /workspace && proto install --yes".to_string(),
        format!(
            "cd {} && proto run {} -- --version > {}",
            shell_single_quote(&app_dir),
            shell_single_quote(runtime_tool),
            shell_single_quote(".tako-runtime-version")
        ),
    ];

    if let Some(install) = install_command.map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("cd /workspace && {}", install));
    }

    if let Some(build) = build_command.map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("cd {} && {}", shell_single_quote(&app_dir), build));
    }

    let has_custom_stages = !stages.is_empty();
    for stage in stages {
        let stage_dir = stage_command_working_dir(&app_dir, stage.working_dir.as_deref())?;
        if let Some(install) = stage
            .install
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            lines.push(format!(
                "cd {} && {}",
                shell_single_quote(&stage_dir),
                install
            ));
        }
        let run = stage.run.trim();
        if run.is_empty() {
            return Err("Build stage run command cannot be empty".to_string());
        }
        lines.push(format!("cd {} && {}", shell_single_quote(&stage_dir), run));
    }

    if !has_preset_commands && !has_custom_stages {
        return Err(
            "Build preset did not define install/build commands and no build stages were configured"
                .to_string(),
        );
    }

    Ok(lines.join(" && "))
}

fn stage_command_working_dir(app_dir: &str, working_dir: Option<&str>) -> Result<String, String> {
    let Some(working_dir) = working_dir.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(app_dir.to_string());
    };
    let normalized = working_dir.replace('\\', "/");
    if normalized.starts_with('/') || normalized.contains("..") {
        return Err(format!(
            "Invalid build stage working_dir for container build: {}",
            working_dir
        ));
    }
    Ok(format!("{}/{}", app_dir, normalized))
}

fn resolve_app_dir(app_subdir: &str) -> Result<(String, String), String> {
    if app_subdir.trim().is_empty() {
        return Ok((String::new(), "/workspace".to_string()));
    }

    let normalized = app_subdir.replace('\\', "/");
    if normalized.starts_with('/') || normalized.contains("..") {
        return Err(format!(
            "Invalid app subdir for container build: {}",
            app_subdir
        ));
    }
    Ok((normalized.clone(), format!("/workspace/{normalized}")))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn dependency_cache_mounts(
    target_label: &str,
    runtime_tool: &str,
    builder_image: &str,
) -> Vec<ContainerCacheMount> {
    let mut mounts = vec![ContainerCacheMount {
        volume_name: dependency_cache_volume_name("proto", target_label, builder_image),
        container_path: PROTO_HOME_PATH.to_string(),
    }];
    if runtime_tool == "bun" {
        mounts.push(ContainerCacheMount {
            volume_name: dependency_cache_volume_name("bun", target_label, builder_image),
            container_path: BUN_INSTALL_CACHE_PATH.to_string(),
        });
    }
    mounts
}

fn dependency_cache_volume_name(kind: &str, target_label: &str, builder_image: &str) -> String {
    let kind = sanitize_volume_component(kind);
    let target = sanitize_volume_component(target_label);
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(target_label.as_bytes());
    hasher.update([0]);
    hasher.update(builder_image.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let short_hash = &hash[..16];
    format!("{CACHE_VOLUME_PREFIX}-{kind}-{target}-{short_hash}")
}

fn sanitize_volume_component(value: &str) -> String {
    let mapped: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let collapsed = mapped
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "default".to_string()
    } else if collapsed.len() > 48 {
        collapsed[..48].to_string()
    } else {
        collapsed
    }
}

pub fn docker_platform_for_target_label(target_label: &str) -> Result<&'static str, String> {
    if !target_label.starts_with("linux-") {
        return Err(format!(
            "Unsupported target '{}': expected linux target label",
            target_label
        ));
    }
    let without_linux = target_label.trim_start_matches("linux-");
    let Some((arch, _libc)) = without_linux.split_once('-') else {
        return Err(format!(
            "Unsupported target '{}': expected linux-<arch>-<libc>",
            target_label
        ));
    };
    match arch {
        "x86_64" => Ok("linux/amd64"),
        "aarch64" => Ok("linux/arm64"),
        other => Err(format!(
            "Unsupported target architecture '{}': supported architectures are x86_64 and aarch64",
            other
        )),
    }
}

fn default_builder_image_for_target_label(target_label: &str) -> Result<&'static str, String> {
    if !target_label.starts_with("linux-") {
        return Err(format!(
            "Unsupported target '{}': expected linux-<arch>-<libc>",
            target_label
        ));
    }
    let without_linux = target_label.trim_start_matches("linux-");
    let Some((_arch, libc)) = without_linux.split_once('-') else {
        return Err(format!(
            "Unsupported target '{}': expected linux-<arch>-<libc>",
            target_label
        ));
    };
    match libc {
        "glibc" => Ok("debian:bookworm-slim"),
        "musl" => Ok("alpine:3.20"),
        other => Err(format!(
            "Unsupported target libc '{}': supported values are glibc and musl",
            other
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_platform_mapping_supports_known_targets() {
        assert_eq!(
            docker_platform_for_target_label("linux-x86_64-glibc").unwrap(),
            "linux/amd64"
        );
        assert_eq!(
            docker_platform_for_target_label("linux-aarch64-musl").unwrap(),
            "linux/arm64"
        );
    }

    #[test]
    fn docker_platform_mapping_rejects_unknown_targets() {
        assert!(docker_platform_for_target_label("darwin-x86_64").is_err());
        assert!(docker_platform_for_target_label("linux-sparc-glibc").is_err());
    }

    #[test]
    fn default_builder_image_mapping_supports_target_libc() {
        assert_eq!(
            default_builder_image_for_target_label("linux-x86_64-glibc").unwrap(),
            "debian:bookworm-slim"
        );
        assert_eq!(
            default_builder_image_for_target_label("linux-aarch64-musl").unwrap(),
            "alpine:3.20"
        );
    }

    #[test]
    fn container_script_uses_workspace_root_for_install_and_app_dir_for_build() {
        let script = build_container_script(
            "apps/web",
            "bun",
            Some("bun install --frozen-lockfile"),
            Some("bun run build"),
            &[],
        )
        .unwrap();
        assert!(script.contains("export TAKO_APP_SUBDIR='apps/web'"));
        assert!(script.contains("export TAKO_APP_DIR='/workspace/apps/web'"));
        assert!(script.contains("export PROTO_HOME='/var/cache/tako/proto'"));
        assert!(script.contains("cd /workspace && proto install --yes"));
        assert!(script.contains("cd /workspace && bun install --frozen-lockfile"));
        assert!(script.contains("cd '/workspace/apps/web' && bun run build"));
    }

    #[test]
    fn docker_create_args_include_platform_image_and_shell_script() {
        let args = build_docker_create_args("linux/amd64", "oven/bun:1.2", "echo hello", &[]);
        assert_eq!(args[0], "create");
        assert!(args.contains(&"--platform".to_string()));
        assert!(args.contains(&"linux/amd64".to_string()));
        assert!(args.contains(&"oven/bun:1.2".to_string()));
        assert_eq!(args[args.len() - 2], "-lc");
        assert_eq!(args[args.len() - 1], "echo hello");
    }

    #[test]
    fn bun_target_enables_bun_dependency_cache_mount() {
        let mounts = dependency_cache_mounts("linux-x86_64-glibc", "bun", "debian:bookworm-slim");

        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].container_path, "/var/cache/tako/proto");
        assert!(mounts[0].volume_name.starts_with("tako-build-cache-proto-"));
        assert_eq!(
            mounts[1].container_path,
            "/var/cache/tako/bun/install/cache"
        );
        assert!(mounts[1].volume_name.starts_with("tako-build-cache-bun-"));
    }

    #[test]
    fn dependency_cache_volume_name_changes_with_target_and_image() {
        let a = dependency_cache_volume_name("bun", "linux-x86_64-glibc", "oven/bun:1.2");
        let b = dependency_cache_volume_name("bun", "linux-aarch64-glibc", "oven/bun:1.2");
        let c = dependency_cache_volume_name("bun", "linux-x86_64-glibc", "oven/bun:latest");

        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn docker_create_args_include_cache_mounts() {
        let mounts = vec![ContainerCacheMount {
            volume_name: "tako-build-cache-bun-linux-x86_64-glibc-abcd1234".to_string(),
            container_path: "/var/cache/tako/bun/install/cache".to_string(),
        }];
        let args = build_docker_create_args("linux/amd64", "oven/bun:1.2", "echo hello", &mounts);
        let expected_mount = "type=volume,src=tako-build-cache-bun-linux-x86_64-glibc-abcd1234,dst=/var/cache/tako/bun/install/cache".to_string();
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--mount" && pair[1] == expected_mount)
        );
    }

    #[test]
    fn container_script_rejects_empty_commands() {
        let err = build_container_script("", "bun", None, None, &[]).unwrap_err();
        assert!(err.contains("no build stages were configured"));
    }

    #[test]
    fn container_script_bootstraps_latest_runtime_when_no_prototools_exists() {
        let script =
            build_container_script("apps/web", "bun", Some("bun install"), None, &[]).unwrap();
        assert!(
            script
                .contains("if [ ! -f /workspace/.prototools ]; then printf '%s = \"latest\"\\n' 'bun' > /workspace/.prototools; fi")
        );
    }

    #[test]
    fn container_script_runs_custom_stages_after_preset_build() {
        let script = build_container_script(
            "apps/web",
            "bun",
            Some("bun install"),
            Some("bun run build"),
            &[BuildStageCommand {
                name: Some("frontend-assets".to_string()),
                working_dir: Some("frontend".to_string()),
                install: Some("bun install".to_string()),
                run: "bun run build".to_string(),
            }],
        )
        .unwrap();
        let preset_index = script
            .find("cd '/workspace/apps/web' && bun run build")
            .expect("preset build command");
        let stage_install_index = script
            .find("cd '/workspace/apps/web/frontend' && bun install")
            .expect("stage install command");
        let stage_run_index = script
            .find("cd '/workspace/apps/web/frontend' && bun run build")
            .expect("stage run command");
        assert!(preset_index < stage_install_index);
        assert!(stage_install_index < stage_run_index);
    }

    #[test]
    fn container_script_accepts_custom_stages_without_preset_commands() {
        let script = build_container_script(
            "apps/web",
            "bun",
            None,
            None,
            &[BuildStageCommand {
                name: None,
                working_dir: None,
                install: None,
                run: "bun run build".to_string(),
            }],
        )
        .unwrap();
        assert!(script.contains("cd '/workspace/apps/web' && bun run build"));
    }
}
