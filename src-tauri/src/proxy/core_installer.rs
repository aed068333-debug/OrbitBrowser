use crate::errors::{AppError, AppResult};
use serde_json::json;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const XRAY_VERSION: &str = "v25.12.8";

pub fn managed_core_dir() -> AppResult<PathBuf> {
    dirs::data_dir()
        .map(|dir| dir.join("com.orbit.browser").join("runtime").join("proxy-core"))
        .ok_or_else(|| AppError::new("data_dir_unavailable", "Unable to resolve app data dir"))
}

pub fn managed_xray_path() -> AppResult<PathBuf> {
    let dir = managed_core_dir()?;
    if cfg!(target_os = "windows") {
        Ok(dir.join("xray.exe"))
    } else {
        Ok(dir.join("xray"))
    }
}


pub async fn ensure_managed_xray() -> AppResult<PathBuf> {
    let path = managed_xray_path()?;
    let version_marker = managed_core_dir()?.join("version.txt");
    let version_ok = std::fs::read_to_string(&version_marker)
        .map(|text| text.lines().next().unwrap_or("").trim() == XRAY_VERSION)
        .unwrap_or(false);
    if path.is_file() && version_ok {
        if Command::new(&path)
            .arg("version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
        {
            return Ok(path);
        }
    }

    install_managed_xray().await
}

pub async fn install_managed_xray() -> AppResult<PathBuf> {
    let target_dir = managed_core_dir()?;
    std::fs::create_dir_all(&target_dir)?;
    let asset = release_asset_name()?;
    let url = format!(
        "https://github.com/XTLS/Xray-core/releases/download/{XRAY_VERSION}/{asset}"
    );

    tracing::info!(%url, "downloading managed xray core");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()?;
    let response = client
        .get(&url)
        .header("User-Agent", "OrbitBrowser/0.5 proxy-core-installer")
        .send()
        .await
        .map_err(|err| {
            AppError::new(
                "proxy_core_install_failed",
                format!("Failed to download xray core: {err}"),
            )
            .details(json!({ "url": url }))
            .retryable(true)
        })?;
    if !response.status().is_success() {
        return Err(AppError::new(
            "proxy_core_install_failed",
            format!("Xray download returned HTTP {}", response.status()),
        )
        .details(json!({ "url": url }))
        .retryable(true));
    }
    let bytes = response.bytes().await.map_err(|err| {
        AppError::new(
            "proxy_core_install_failed",
            format!("Failed to read xray package: {err}"),
        )
        .retryable(true)
    })?;

    let binary_name = if cfg!(target_os = "windows") {
        "xray.exe"
    } else {
        "xray"
    };
    let extracted = extract_named_file_from_zip(&bytes, binary_name)?;
    let target = managed_xray_path()?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // atomic-ish replace
    let tmp = target.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&extracted)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms)?;
    }
    std::fs::rename(&tmp, &target)?;

    // keep geo files optional: ignore
    let version_ok = Command::new(&target)
        .arg("version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !version_ok {
        return Err(AppError::new(
            "proxy_core_install_failed",
            "Installed xray binary failed version check",
        )
        .details(json!({ "path": target }))
        .retryable(true));
    }

    // record version marker
    let _ = std::fs::write(
        target_dir.join("version.txt"),
        format!("{XRAY_VERSION}\n{asset}\n"),
    );
    Ok(target)
}

fn release_asset_name() -> AppResult<&'static str> {
    let arch = std::env::consts::ARCH;
    match (std::env::consts::OS, arch) {
        ("macos", "aarch64") => Ok("Xray-macos-arm64-v8a.zip"),
        ("macos", "x86_64") => Ok("Xray-macos-64.zip"),
        ("linux", "aarch64") => Ok("Xray-linux-arm64-v8a.zip"),
        ("linux", "x86_64") => Ok("Xray-linux-64.zip"),
        ("windows", "aarch64") => Ok("Xray-windows-arm64-v8a.zip"),
        ("windows", "x86_64") => Ok("Xray-windows-64.zip"),
        (os, arch) => Err(AppError::new(
            "proxy_core_install_failed",
            format!("Unsupported platform for managed xray: {os}/{arch}"),
        )),
    }
}

fn extract_named_file_from_zip(bytes: &[u8], file_name: &str) -> AppResult<Vec<u8>> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|err| {
        AppError::new(
            "proxy_core_install_failed",
            format!("Invalid xray zip package: {err}"),
        )
    })?;

    // exact name first
    if let Ok(mut file) = archive.by_name(file_name) {
        let mut out = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut out)?;
        return Ok(out);
    }

    // fallback: any path ending with file name
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|err| {
            AppError::new(
                "proxy_core_install_failed",
                format!("Failed to read zip entry: {err}"),
            )
        })?;
        let name = file.name().replace('\\', "/");
        if name.rsplit('/').next() == Some(file_name) && !name.ends_with('/') {
            let mut out = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut out)?;
            return Ok(out);
        }
    }

    Err(AppError::new(
        "proxy_core_install_failed",
        format!("Zip package does not contain {file_name}"),
    ))
}

#[allow(dead_code)]
pub fn managed_core_exists(path: &Path) -> bool {
    path.is_file()
}
