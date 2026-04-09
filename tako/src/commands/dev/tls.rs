use std::path::{Path, PathBuf};

use crate::dev::LocalCA;

const DEV_TLS_CERT_FILENAME: &str = "fullchain.pem";
const DEV_TLS_KEY_FILENAME: &str = "privkey.pem";
const DEV_TLS_NAMES_FILENAME: &str = "names.json";

pub(super) fn dev_server_tls_paths_for_home(home: &Path) -> (PathBuf, PathBuf) {
    let certs_dir = home.join("certs");
    (
        certs_dir.join(DEV_TLS_CERT_FILENAME),
        certs_dir.join(DEV_TLS_KEY_FILENAME),
    )
}

pub(super) fn dev_server_tls_names_path_for_home(home: &Path) -> PathBuf {
    home.join("certs").join(DEV_TLS_NAMES_FILENAME)
}

fn default_dev_tls_names_for_app(app_name: &str) -> Vec<String> {
    let d = crate::dev::TAKO_DEV_DOMAIN;
    let s = crate::dev::SHORT_DEV_DOMAIN;
    vec![
        format!("*.{s}"),
        s.to_string(),
        format!("{app_name}.{s}"),
        format!("*.{app_name}.{s}"),
        format!("*.{d}"),
        d.to_string(),
        format!("{app_name}.{d}"),
        format!("*.{app_name}.{d}"),
    ]
}

fn normalize_tls_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

fn load_dev_tls_names(path: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<Vec<String>>(&raw).ok()?;
    Some(normalize_tls_names(parsed))
}

pub(super) fn ensure_dev_server_tls_material_for_home(
    ca: &LocalCA,
    home: &Path,
    app_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let (cert_path, key_path) = dev_server_tls_paths_for_home(home);
    let names_path = dev_server_tls_names_path_for_home(home);
    let have_cert_material = cert_path.is_file() && key_path.is_file();
    let existing_names = if have_cert_material {
        load_dev_tls_names(&names_path)
    } else {
        None
    };
    let mut names = default_dev_tls_names_for_app(app_name);
    if let Some(existing) = existing_names.clone() {
        names.extend(existing);
    }
    let names = normalize_tls_names(names);

    if have_cert_material
        && existing_names
            .as_ref()
            .is_some_and(|existing| *existing == names)
    {
        return Ok(false);
    }

    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let name_refs: Vec<&str> = names.iter().map(|name| name.as_str()).collect();
    let cert = ca.generate_leaf_cert_for_names(&name_refs)?;
    std::fs::write(&cert_path, cert.cert_pem.as_bytes())?;
    std::fs::write(&key_path, cert.key_pem.as_bytes())?;
    std::fs::write(&names_path, serde_json::to_string_pretty(&names)?)?;
    Ok(true)
}

pub(super) fn ensure_dev_server_tls_material(
    ca: &LocalCA,
    app_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let data_dir = crate::paths::tako_data_dir()?;
    ensure_dev_server_tls_material_for_home(ca, &data_dir, app_name)
}
