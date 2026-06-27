//! winget-pkgs installs.
//! Uses the public GitHub API / raw.githubusercontent.com — no winget.exe.

use crate::bottle::BottleDownloader;
use crate::error::{Result, WaxError};
use crate::package_spec::Ecosystem;
use crate::scoop;
use crate::version;
use crate::windows_state::{self, WindowsNativeUninstall, WindowsPackageManifest};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tracing::debug;

const WINGET_PKGS_REPO_CONTENTS: &str =
    "https://api.github.com/repos/microsoft/winget-pkgs/contents";
const WINGET_PKGS_RAW: &str = "https://raw.githubusercontent.com/microsoft/winget-pkgs/master";

#[derive(Debug, Deserialize)]
struct GhContentEntry {
    name: String,
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
}

fn package_id_to_content_path(id: &str) -> Result<String> {
    let parts: Vec<&str> = id.split('.').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return Err(WaxError::InvalidInput(
            "winget PackageIdentifier needs at least two dot-separated segments (e.g. JesseDuffield.lazygit)"
                .into(),
        ));
    }
    let first = parts[0]
        .chars()
        .next()
        .ok_or_else(|| WaxError::InvalidInput("empty winget id".into()))?
        .to_ascii_lowercase();
    Ok(format!("manifests/{}/{}", first, parts.join("/")))
}

fn github_client() -> &'static reqwest::Client {
    crate::http_client::default_client()
}

async fn gh_get_json(url: &str) -> Result<Vec<GhContentEntry>> {
    gh_list_dir_url(url)
        .await?
        .ok_or_else(|| WaxError::InstallError(format!("GitHub API {url} -> HTTP 404 Not Found")))
}

async fn gh_list_dir_url(url: &str) -> Result<Option<Vec<GhContentEntry>>> {
    let client = github_client();
    let resp = client.get(url).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(WaxError::InstallError(format!(
            "GitHub API {} -> HTTP {}",
            url,
            resp.status()
        )));
    }
    let v: serde_json::Value = resp.json().await?;
    if v.is_array() {
        Ok(Some(
            serde_json::from_value(v).map_err(WaxError::JsonError)?,
        ))
    } else {
        Err(WaxError::InstallError(
            "Unexpected GitHub API response (expected directory listing)".into(),
        ))
    }
}

async fn gh_list_dir(rel_path: &str) -> Result<Option<Vec<GhContentEntry>>> {
    let url = format!("{WINGET_PKGS_REPO_CONTENTS}/{rel_path}?ref=master");
    gh_list_dir_url(&url).await
}

/// Resolve a winget PackageIdentifier to the canonical casing used in winget-pkgs.
async fn resolve_winget_package_id(package_id: &str) -> Result<String> {
    let rel = package_id_to_content_path(package_id)?;
    if gh_list_dir(&rel).await?.is_some() {
        return Ok(package_id.to_string());
    }

    let parts: Vec<&str> = package_id.split('.').filter(|s| !s.is_empty()).collect();
    let first_letter = parts[0]
        .chars()
        .next()
        .ok_or_else(|| WaxError::InvalidInput("empty winget id".into()))?
        .to_ascii_lowercase();
    let letter_url = format!("{WINGET_PKGS_REPO_CONTENTS}/manifests/{first_letter}?ref=master");
    let publishers = gh_get_json(&letter_url).await?;
    let publisher = publishers
        .iter()
        .find(|e| e.entry_type == "dir" && e.name.eq_ignore_ascii_case(parts[0]))
        .ok_or_else(|| {
            WaxError::FormulaNotFound(format!(
                "winget package '{package_id}' not found in winget-pkgs (try `wax search winget {package_id}`)"
            ))
        })?;

    let mut canonical = vec![publisher.name.clone()];
    let mut current_path = publisher.path.clone();
    for part in parts.iter().skip(1) {
        let entries = gh_get_json(&format!(
            "{WINGET_PKGS_REPO_CONTENTS}/{current_path}?ref=master"
        ))
        .await?;
        let matched = entries
            .iter()
            .find(|e| e.entry_type == "dir" && e.name.eq_ignore_ascii_case(part))
            .ok_or_else(|| {
                WaxError::FormulaNotFound(format!(
                    "winget package '{package_id}' not found in winget-pkgs (try `wax search winget {package_id}`)"
                ))
            })?;
        canonical.push(matched.name.clone());
        current_path = matched.path.clone();
    }

    Ok(canonical.join("."))
}

/// True if `microsoft/winget-pkgs` has a manifest directory for this PackageIdentifier.
#[cfg(target_os = "windows")]
pub async fn winget_package_exists(package_id: &str) -> bool {
    resolve_winget_package_id(package_id).await.is_ok()
}

fn winget_arch_token() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "x64",
    }
}

fn join_under_root(root: &Path, rel: &Path) -> Result<PathBuf> {
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WaxError::InstallError(format!(
                    "Unsafe path in winget manifest: {}",
                    rel.display()
                )));
            }
        }
    }
    Ok(root.join(rel))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WingetInstallerDoc {
    installer_type: Option<String>,
    nested_installer_type: Option<String>,
    nested_installer_files: Option<Vec<WingetNestedFile>>,
    installer_switches: Option<WingetInstallerSwitches>,
    apps_and_features_entries: Option<Vec<WingetAppsAndFeaturesEntry>>,
    installers: Vec<WingetInstallerEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WingetNestedFile {
    relative_file_path: String,
    portable_command_alias: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WingetInstallerEntry {
    architecture: String,
    installer_url: String,
    installer_sha256: String,
    installer_type: Option<String>,
    installer_switches: Option<WingetInstallerSwitches>,
    product_code: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WingetInstallerSwitches {
    silent: Option<String>,
    silent_with_progress: Option<String>,
    custom: Option<String>,
    install_location: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WingetAppsAndFeaturesEntry {
    product_code: Option<String>,
    package_family_name: Option<String>,
    silent_uninstall_string: Option<String>,
}

fn pick_installer(doc: &WingetInstallerDoc) -> Result<&WingetInstallerEntry> {
    let want = winget_arch_token();
    doc.installers
        .iter()
        .find(|i| i.architecture.eq_ignore_ascii_case(want))
        .or_else(|| doc.installers.first())
        .ok_or_else(|| WaxError::InstallError("winget manifest has no installers".into()))
}

fn installer_type_for<'a>(doc: &'a WingetInstallerDoc, inst: &'a WingetInstallerEntry) -> &'a str {
    inst.installer_type
        .as_deref()
        .or(doc.installer_type.as_deref())
        .unwrap_or("")
}

fn installer_switches_for<'a>(
    doc: &'a WingetInstallerDoc,
    inst: &'a WingetInstallerEntry,
) -> Option<&'a WingetInstallerSwitches> {
    inst.installer_switches
        .as_ref()
        .or(doc.installer_switches.as_ref())
}

fn split_switches(raw: &str) -> Vec<String> {
    raw.split_whitespace().map(|s| s.to_string()).collect()
}

fn msi_install_args(path: &Path) -> Vec<String> {
    vec![
        "/i".into(),
        path.to_string_lossy().to_string(),
        "/qn".into(),
        "/norestart".into(),
    ]
}

fn msi_uninstall(product_code: &str) -> WindowsNativeUninstall {
    WindowsNativeUninstall {
        command: "msiexec.exe".into(),
        args: vec![
            "/x".into(),
            product_code.to_string(),
            "/qn".into(),
            "/norestart".into(),
        ],
    }
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn msix_install_args(path: &Path) -> Vec<String> {
    vec![
        "-NoProfile".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-Command".into(),
        format!(
            "Add-AppxPackage -Path {}",
            ps_quote(&path.to_string_lossy())
        ),
    ]
}

fn msix_uninstall(package_family_name: &str) -> WindowsNativeUninstall {
    WindowsNativeUninstall {
        command: "powershell.exe".into(),
        args: vec![
            "-NoProfile".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            format!(
                "Get-AppxPackage -PackageFamilyName {} | Remove-AppxPackage",
                ps_quote(package_family_name)
            ),
        ],
    }
}

fn exe_install_args(switches: &WingetInstallerSwitches) -> Result<Vec<String>> {
    let raw = switches
        .silent
        .as_deref()
        .or(switches.silent_with_progress.as_deref())
        .ok_or_else(|| {
            WaxError::InstallError("winget exe installer is missing silent install switches".into())
        })?;
    let mut args = split_switches(raw);
    if let Some(custom) = &switches.custom {
        args.extend(split_switches(custom));
    }
    if let Some(location) = &switches.install_location {
        args.extend(split_switches(location));
    }
    Ok(args)
}

fn exe_uninstall(doc: &WingetInstallerDoc) -> Result<WindowsNativeUninstall> {
    let raw = doc
        .apps_and_features_entries
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find_map(|entry| entry.silent_uninstall_string.as_deref())
        .ok_or_else(|| {
            WaxError::InstallError(
                "winget exe installer is missing silent uninstall metadata".into(),
            )
        })?;
    let mut parts = split_switches(raw);
    let command = parts
        .drain(..1)
        .next()
        .ok_or_else(|| WaxError::InstallError("empty silent uninstall metadata".into()))?;
    Ok(WindowsNativeUninstall {
        command,
        args: parts,
    })
}

fn uninstall_for_exe_like(
    doc: &WingetInstallerDoc,
    inst: &WingetInstallerEntry,
) -> Result<WindowsNativeUninstall> {
    if let Ok(uninstall) = exe_uninstall(doc) {
        return Ok(uninstall);
    }
    let product_code = product_code_for(doc, inst).ok_or_else(|| {
        WaxError::InstallError(
            "winget exe installer is missing silent uninstall metadata and ProductCode".into(),
        )
    })?;
    Ok(msi_uninstall(&product_code))
}

fn product_code_for(doc: &WingetInstallerDoc, inst: &WingetInstallerEntry) -> Option<String> {
    inst.product_code.clone().or_else(|| {
        doc.apps_and_features_entries
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find_map(|entry| entry.product_code.clone())
    })
}

fn package_family_name_for(doc: &WingetInstallerDoc) -> Option<String> {
    doc.apps_and_features_entries
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find_map(|entry| entry.package_family_name.clone())
}

fn native_plan(
    doc: &WingetInstallerDoc,
    inst: &WingetInstallerEntry,
    installer_path: &Path,
) -> Result<(String, Vec<String>, WindowsNativeUninstall)> {
    let inst_type = installer_type_for(doc, inst).to_ascii_lowercase();
    match inst_type.as_str() {
        "msi" | "wix" => {
            let product_code = product_code_for(doc, inst).ok_or_else(|| {
                WaxError::InstallError(
                    "winget msi installer is missing ProductCode for managed uninstall".into(),
                )
            })?;
            Ok((
                "msiexec.exe".into(),
                msi_install_args(installer_path),
                msi_uninstall(&product_code),
            ))
        }
        "msix" | "appx" => {
            let family = package_family_name_for(doc).ok_or_else(|| {
                WaxError::InstallError(
                    "winget msix installer is missing PackageFamilyName for managed uninstall"
                        .into(),
                )
            })?;
            Ok((
                "powershell.exe".into(),
                msix_install_args(installer_path),
                msix_uninstall(&family),
            ))
        }
        "exe" | "inno" | "nullsoft" | "burn" => {
            let switches = installer_switches_for(doc, inst).ok_or_else(|| {
                WaxError::InstallError("winget exe installer is missing InstallerSwitches".into())
            })?;
            Ok((
                installer_path.to_string_lossy().to_string(),
                exe_install_args(switches)?,
                uninstall_for_exe_like(doc, inst)?,
            ))
        }
        _ => Err(WaxError::InstallError(format!(
            "wax does not support native winget InstallerType={inst_type}"
        ))),
    }
}

fn run_native_command(command: &str, args: &[String]) -> Result<()> {
    if !cfg!(target_os = "windows") {
        return Err(WaxError::PlatformNotSupported(
            "native Windows installer execution is only supported on Windows".into(),
        ));
    }
    let status = Command::new(command).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(WaxError::InstallError(format!(
            "native installer command failed with {status}: {command}"
        )))
    }
}

pub async fn install_winget_package(package_id: &str) -> Result<()> {
    if !cfg!(target_os = "windows") {
        return Err(WaxError::PlatformNotSupported(
            "winget install is only supported on Windows".into(),
        ));
    }

    let package_id = resolve_winget_package_id(package_id).await?;
    let rel = package_id_to_content_path(&package_id)?;
    let entries = gh_get_json(&format!("{WINGET_PKGS_REPO_CONTENTS}/{rel}?ref=master")).await?;

    let mut versions: Vec<String> = entries
        .iter()
        .filter(|e| e.entry_type == "dir")
        .map(|e| e.name.clone())
        .collect();
    if versions.is_empty() {
        return Err(WaxError::FormulaNotFound(format!(
            "no version folders under winget-pkgs/{rel}"
        )));
    }
    version::sort_versions(&mut versions);
    let latest = versions
        .last()
        .ok_or_else(|| WaxError::InstallError("no winget versions".into()))?
        .clone();

    let ver_url = format!("{WINGET_PKGS_REPO_CONTENTS}/{rel}/{latest}?ref=master");
    let files = gh_get_json(&ver_url).await?;
    let installer_yaml = files
        .iter()
        .find(|e| e.name.ends_with(".installer.yaml") && e.entry_type == "file")
        .ok_or_else(|| {
            WaxError::InstallError(
                "No .installer.yaml in latest winget version (wax only supports installer manifests)"
                    .into(),
            )
        })?;

    let yaml_path = &installer_yaml.path;
    let raw_url = format!("{WINGET_PKGS_RAW}/{yaml_path}");
    debug!("Fetching winget installer yaml {}", raw_url);
    let yaml_text = github_client()
        .get(&raw_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let doc: WingetInstallerDoc =
        serde_yaml::from_str(&yaml_text).map_err(|e| WaxError::ParseError(e.to_string()))?;

    let inst = pick_installer(&doc)?;
    let inst_type = installer_type_for(&doc, inst);
    let nested = doc.nested_installer_type.as_deref().unwrap_or("");
    let sha_expected = inst.installer_sha256.trim().to_ascii_lowercase();

    let tmp = TempDir::new()?;
    let archive_name = Path::new(&inst.installer_url)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("winget-installer.bin");
    let archive_path = tmp.path().join(archive_name);

    let dl = BottleDownloader::new();
    let size = dl.probe_size(&inst.installer_url).await;
    let conns =
        BottleDownloader::num_connections(size, BottleDownloader::MAX_CONNECTIONS_PER_DOWNLOAD);
    let pb = ProgressBar::new(0);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.cyan} {msg} [{bar:30.cyan/blue}] {bytes}/{total_bytes}")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_message(format!("{} {}", package_id, latest));

    dl.download(&inst.installer_url, &archive_path, Some(&pb), conns, None)
        .await?;
    pb.finish_and_clear();

    BottleDownloader::verify_checksum(&archive_path, &sha_expected)?;

    if !inst_type.eq_ignore_ascii_case("zip") || !nested.eq_ignore_ascii_case("portable") {
        return install_native_winget_package(&package_id, &latest, &doc, inst, &archive_path)
            .await;
    }

    let extract_root = tmp.path().join("extract");
    std::fs::create_dir_all(&extract_root)?;
    scoop::extract_zip_file(&archive_path, &extract_root)?;

    let bin_dir = windows_state::wax_bin_dir()?;
    std::fs::create_dir_all(&bin_dir)?;

    let nested_files = doc.nested_installer_files.as_ref().ok_or_else(|| {
        WaxError::InstallError("winget manifest missing NestedInstallerFiles".into())
    })?;

    let mut copy_actions = Vec::new();
    for nf in nested_files {
        let rel = PathBuf::from(nf.relative_file_path.replace('\\', "/"));
        let src = join_under_root(&extract_root, &rel)?;
        if !src.exists() {
            return Err(WaxError::InstallError(format!(
                "nested portable file missing: {}",
                src.display()
            )));
        }
        let dest_name = nf
            .portable_command_alias
            .as_ref()
            .map(|s| format!("{s}.exe"))
            .unwrap_or_else(|| {
                src.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "app.exe".into())
            });
        let dest = bin_dir.join(dest_name);
        copy_actions.push((src, dest));
    }
    let bin_links: Vec<PathBuf> = copy_actions.iter().map(|(_, dest)| dest.clone()).collect();
    windows_state::validate_bin_links_available(Ecosystem::Winget, &package_id, &bin_links)?;

    for (src, dest) in copy_actions {
        if dest.exists() {
            let _ = std::fs::remove_file(&dest);
        }
        std::fs::copy(&src, &dest)?;
    }

    let staging = windows_state::wax_windows_root()?
        .join("winget-apps")
        .join(package_id.replace('.', "_"))
        .join(&latest);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(staging.parent().unwrap())?;
    crate::ui::copy_dir_all(&extract_root, &staging)?;

    let mut files = windows_state::collect_files(&staging)?;
    files.extend(bin_links.iter().cloned());
    WindowsPackageManifest::new(
        Ecosystem::Winget,
        package_id,
        latest.clone(),
        inst.installer_url.clone(),
        staging.clone(),
        bin_links,
        files,
    )
    .save()?;

    println!(
        "Installed {} {} (winget portable zip) — binaries under:\n  {}",
        package_id,
        latest,
        bin_dir.display()
    );

    Ok(())
}

async fn install_native_winget_package(
    package_id: &str,
    latest: &str,
    doc: &WingetInstallerDoc,
    inst: &WingetInstallerEntry,
    installer_path: &Path,
) -> Result<()> {
    let staging = windows_state::wax_windows_root()?
        .join("winget-installers")
        .join(package_id.replace('.', "_"))
        .join(latest);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    let staged_installer = staging.join(
        installer_path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("installer.bin")),
    );
    let (command, args, uninstall) = native_plan(doc, inst, &staged_installer)?;
    std::fs::create_dir_all(&staging)?;
    std::fs::copy(installer_path, &staged_installer)?;

    if let Err(err) = run_native_command(&command, &args) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }

    let files = windows_state::collect_files(&staging)?;
    let manifest = WindowsPackageManifest::new(
        Ecosystem::Winget,
        package_id,
        latest.to_string(),
        inst.installer_url.clone(),
        staging.clone(),
        Vec::new(),
        files,
    )
    .with_native_uninstall(uninstall);
    if let Err(err) = manifest.save() {
        if let Some(native) = &manifest.native_uninstall {
            let _ = run_native_command(&native.command, &native.args);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }

    println!(
        "Installed {} {} (winget native installer)",
        package_id, latest
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(installer_type: &str) -> WingetInstallerEntry {
        WingetInstallerEntry {
            architecture: "x64".into(),
            installer_url: "https://example.invalid/app.msi".into(),
            installer_sha256: "abc".into(),
            installer_type: Some(installer_type.into()),
            installer_switches: None,
            product_code: None,
        }
    }

    #[test]
    fn native_msi_requires_product_code_and_builds_msiexec() {
        let mut inst = entry("msi");
        inst.product_code = Some("{PRODUCT}".into());
        let doc = WingetInstallerDoc {
            installer_type: None,
            nested_installer_type: None,
            nested_installer_files: None,
            installer_switches: None,
            apps_and_features_entries: None,
            installers: vec![],
        };
        let (cmd, args, uninstall) = native_plan(&doc, &inst, Path::new("C:/tmp/app.msi")).unwrap();
        assert_eq!(cmd, "msiexec.exe");
        assert_eq!(args[0], "/i");
        assert_eq!(uninstall.command, "msiexec.exe");
        assert_eq!(uninstall.args[0], "/x");
        assert_eq!(uninstall.args[1], "{PRODUCT}");
    }

    #[test]
    fn native_exe_rejects_missing_uninstall_metadata() {
        let mut inst = entry("exe");
        inst.installer_switches = Some(WingetInstallerSwitches {
            silent: Some("/S".into()),
            silent_with_progress: None,
            custom: None,
            install_location: None,
        });
        let doc = WingetInstallerDoc {
            installer_type: None,
            nested_installer_type: None,
            nested_installer_files: None,
            installer_switches: None,
            apps_and_features_entries: None,
            installers: vec![],
        };
        assert!(native_plan(&doc, &inst, Path::new("C:/tmp/app.exe")).is_err());
    }

    #[test]
    fn native_exe_falls_back_to_product_code_uninstall() {
        let mut inst = entry("exe");
        inst.installer_switches = Some(WingetInstallerSwitches {
            silent: Some("/quiet /norestart".into()),
            silent_with_progress: None,
            custom: None,
            install_location: None,
        });
        inst.product_code = Some("{ITUNES}".into());
        let doc = WingetInstallerDoc {
            installer_type: Some("exe".into()),
            nested_installer_type: None,
            nested_installer_files: None,
            installer_switches: Some(WingetInstallerSwitches {
                silent: Some("/quiet /norestart".into()),
                silent_with_progress: None,
                custom: None,
                install_location: None,
            }),
            apps_and_features_entries: Some(vec![WingetAppsAndFeaturesEntry {
                product_code: Some("{ITUNES}".into()),
                package_family_name: None,
                silent_uninstall_string: None,
            }]),
            installers: vec![],
        };
        let (cmd, args, uninstall) =
            native_plan(&doc, &inst, Path::new("C:/tmp/iTunes64Setup.exe")).unwrap();
        assert_eq!(cmd, "C:/tmp/iTunes64Setup.exe");
        assert_eq!(args, vec!["/quiet", "/norestart"]);
        assert_eq!(uninstall.command, "msiexec.exe");
        assert_eq!(uninstall.args[1], "{ITUNES}");
    }

    #[test]
    fn native_msix_builds_powershell_commands() {
        let inst = entry("msix");
        let doc = WingetInstallerDoc {
            installer_type: None,
            nested_installer_type: None,
            nested_installer_files: None,
            installer_switches: None,
            apps_and_features_entries: Some(vec![WingetAppsAndFeaturesEntry {
                product_code: None,
                package_family_name: Some("Example.App_123".into()),
                silent_uninstall_string: None,
            }]),
            installers: vec![],
        };
        let (cmd, args, uninstall) =
            native_plan(&doc, &inst, Path::new("C:/tmp/app.msix")).unwrap();
        assert_eq!(cmd, "powershell.exe");
        assert!(args.iter().any(|arg| arg.contains("Add-AppxPackage")));
        assert_eq!(uninstall.command, "powershell.exe");
        assert!(uninstall
            .args
            .iter()
            .any(|arg| arg.contains("Remove-AppxPackage")));
    }
}
