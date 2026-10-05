use crate::api::CaskArtifact;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::debug;

#[derive(Debug, Default, PartialEq)]
pub struct Script {
    pub executable: String,
    pub args: Vec<String>,
    pub sudo: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct Directives {
    pub early_script: Vec<Script>,
    pub launchctl: Vec<String>,
    pub quit: Vec<String>,
    pub signal: Vec<(String, String)>,
    pub login_item: Vec<String>,
    pub script: Vec<Script>,
    pub pkgutil: Vec<String>,
    pub delete: Vec<String>,
    pub trash: Vec<String>,
    pub rmdir: Vec<String>,
}

impl Directives {
    pub fn is_empty(&self) -> bool {
        *self == Directives::default()
    }
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

fn scripts(value: &Value) -> Vec<Script> {
    let one = |v: &Value| match v {
        Value::String(s) => Some(Script {
            executable: s.clone(),
            ..Script::default()
        }),
        Value::Object(map) => Some(Script {
            executable: map.get("executable")?.as_str()?.to_string(),
            args: map.get("args").map(strings).unwrap_or_default(),
            sudo: map.get("sudo").and_then(Value::as_bool).unwrap_or(false),
        }),
        _ => None,
    };
    match value {
        Value::Array(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

pub fn parse(artifacts: &[CaskArtifact]) -> Directives {
    let mut d = Directives::default();
    for artifact in artifacts {
        let CaskArtifact::Uninstall { uninstall } = artifact else {
            continue;
        };
        for entry in uninstall {
            let Some(map) = entry.as_object() else {
                continue;
            };
            for (key, value) in map {
                match key.as_str() {
                    "early_script" => d.early_script.extend(scripts(value)),
                    "launchctl" => d.launchctl.extend(strings(value)),
                    "quit" => d.quit.extend(strings(value)),
                    "signal" => {
                        if let Value::Array(items) = value {
                            let pairs: Vec<&Value> = if items.first().is_some_and(Value::is_array) {
                                items.iter().collect()
                            } else {
                                vec![value]
                            };
                            for pair in pairs {
                                let parts = strings(pair);
                                if let [signal, bundle] = parts.as_slice() {
                                    d.signal.push((signal.clone(), bundle.clone()));
                                }
                            }
                        }
                    }
                    "login_item" => d.login_item.extend(strings(value)),
                    "script" => d.script.extend(scripts(value)),
                    "pkgutil" => d.pkgutil.extend(strings(value)),
                    "delete" => d.delete.extend(strings(value)),
                    "trash" => d.trash.extend(strings(value)),
                    "rmdir" => d.rmdir.extend(strings(value)),
                    other => debug!("unsupported uninstall directive {}", other),
                }
            }
        }
    }
    d
}

fn expand(path: &str) -> Option<PathBuf> {
    let expanded = PathBuf::from(shellexpand::tilde(path).into_owned());
    let safe = expanded.is_absolute()
        && !expanded
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    safe.then_some(expanded)
}

pub fn is_protected(path: &Path) -> bool {
    let depth = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if depth <= 1 {
        return true;
    }
    let home = crate::ui::dirs::home_dir().ok();
    let mut protected: Vec<PathBuf> = [
        "/Applications",
        "/Library",
        "/Library/Application Support",
        "/Library/LaunchAgents",
        "/Library/LaunchDaemons",
        "/Library/Preferences",
        "/Library/PrivilegedHelperTools",
        "/System",
        "/Users",
        "/private/etc",
        "/private/var",
        "/usr/local",
        "/usr/local/bin",
        "/opt/homebrew",
        "/opt/homebrew/bin",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = &home {
        for sub in [
            "",
            "Library",
            "Library/Application Support",
            "Library/Caches",
            "Library/Preferences",
            "Library/LaunchAgents",
            "Library/Containers",
            "Applications",
            "Desktop",
            "Documents",
            "Downloads",
            ".config",
            ".local",
        ] {
            protected.push(if sub.is_empty() {
                home.clone()
            } else {
                home.join(sub)
            });
        }
    }
    let path = path.components().collect::<PathBuf>();
    protected.contains(&path)
}

fn glob(pattern: &Path) -> Vec<PathBuf> {
    let Some(name) = pattern.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let Some((prefix, suffix)) = name.split_once('*') else {
        return if std::fs::symlink_metadata(pattern).is_ok() {
            vec![pattern.to_path_buf()]
        } else {
            Vec::new()
        };
    };
    let Some(parent) = pattern.parent() else {
        return Vec::new();
    };
    if parent.to_string_lossy().contains('*') || suffix.contains('*') {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(prefix) && n.ends_with(suffix)
        })
        .map(|e| e.path())
        .collect()
}

fn run_quiet(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sudo(args: &[&str]) -> bool {
    crate::sudo::acquire_sudo().is_ok() && run_quiet("sudo", args)
}

fn remove_path(path: &Path, warnings: &mut Vec<String>) {
    if is_protected(path) {
        warnings.push(format!(
            "refusing to delete protected path {}",
            path.display()
        ));
        return;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(path).is_ok()
    } else {
        std::fs::remove_file(path).is_ok()
    };
    if !removed && crate::sudo::sudo_remove(path).is_err() {
        warnings.push(format!("could not delete {}", path.display()));
    }
}

fn trash_path(path: &Path, warnings: &mut Vec<String>) {
    if is_protected(path) {
        warnings.push(format!(
            "refusing to trash protected path {}",
            path.display()
        ));
        return;
    }
    let Some(trash) = crate::ui::dirs::home_dir().ok().map(|h| h.join(".Trash")) else {
        return;
    };
    let Some(name) = path.file_name() else {
        return;
    };
    let _ = std::fs::create_dir_all(&trash);
    let mut dest = trash.join(name);
    let mut n = 1;
    while std::fs::symlink_metadata(&dest).is_ok() {
        dest = trash.join(format!("{} {}", name.to_string_lossy(), n));
        n += 1;
    }
    if std::fs::rename(path, &dest).is_err() {
        remove_path(path, warnings);
    }
}

fn run_script(script: &Script, warnings: &mut Vec<String>) {
    let Some(executable) = expand(&script.executable) else {
        warnings.push(format!(
            "skipping script with relative path {}",
            script.executable
        ));
        return;
    };
    let exe = executable.to_string_lossy().into_owned();
    let mut argv: Vec<&str> = vec![exe.as_str()];
    argv.extend(script.args.iter().map(String::as_str));
    let ok = if script.sudo {
        sudo(&argv)
    } else {
        run_quiet(argv[0], &argv[1..])
    };
    if !ok {
        warnings.push(format!("uninstall script {} failed", exe));
    }
}

fn remove_launchctl(label: &str) {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if !uid.is_empty() {
        let _ = run_quiet("launchctl", &["bootout", &format!("gui/{uid}/{label}")]);
    }
    if run_quiet("launchctl", &["print", &format!("system/{label}")]) {
        let _ = sudo(&["launchctl", "bootout", &format!("system/{label}")]);
    }
    let mut plists = vec![
        PathBuf::from(format!("/Library/LaunchAgents/{label}.plist")),
        PathBuf::from(format!("/Library/LaunchDaemons/{label}.plist")),
    ];
    if let Ok(home) = crate::ui::dirs::home_dir() {
        plists.push(home.join(format!("Library/LaunchAgents/{label}.plist")));
    }
    let mut ignored = Vec::new();
    for plist in plists {
        remove_path(&plist, &mut ignored);
    }
}

fn pkg_location(id: &str) -> PathBuf {
    let info = Command::new("pkgutil").args(["--pkg-info", id]).output();
    let mut volume = PathBuf::from("/");
    let mut location = PathBuf::new();
    if let Ok(out) = info {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(v) = line.strip_prefix("volume: ") {
                volume = PathBuf::from(v.trim());
            } else if let Some(l) = line.strip_prefix("location: ") {
                location = PathBuf::from(l.trim());
            }
        }
    }
    volume.join(location)
}

fn pkgutil_list(id: &str, kind: &str) -> Vec<String> {
    Command::new("pkgutil")
        .args([kind, "--files", id])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn remove_pkg(id: &str, warnings: &mut Vec<String>) {
    let root = pkg_location(id);
    for file in pkgutil_list(id, "--only-files") {
        let path = root.join(&file);
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            continue;
        }
        remove_path(&path, warnings);
    }
    let mut dirs = pkgutil_list(id, "--only-dirs");
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    for dir in dirs {
        let path = root.join(&dir);
        if is_protected(&path) || std::fs::remove_dir(&path).is_ok() {
            continue;
        }
        if std::fs::read_dir(&path).is_ok_and(|mut e| e.next().is_none()) {
            let _ = sudo(&["rmdir", &path.to_string_lossy()]);
        }
    }
    if !run_quiet("pkgutil", &["--forget", id]) {
        let _ = sudo(&["pkgutil", "--forget", id]);
    }
}

fn matching_pkgs(pattern: &str) -> Vec<String> {
    Command::new("pkgutil")
        .arg(format!("--pkgs={pattern}"))
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn apple_script_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn run(d: &Directives) -> Vec<String> {
    let mut warnings = Vec::new();
    for script in &d.early_script {
        run_script(script, &mut warnings);
    }
    for label in &d.launchctl {
        remove_launchctl(label);
    }
    for bundle in &d.quit {
        let id = apple_script_string(bundle);
        let running = Command::new("osascript")
            .args(["-e", &format!("application id \"{id}\" is running")])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
        if running {
            let _ = run_quiet(
                "osascript",
                &["-e", &format!("tell application id \"{id}\" to quit")],
            );
        }
    }
    for (signal, bundle) in &d.signal {
        let id = apple_script_string(bundle);
        let pid = Command::new("osascript")
            .args([
                "-e",
                &format!("tell application \"System Events\" to get unix id of every process whose bundle identifier is \"{id}\""),
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        for pid in pid
            .split(", ")
            .filter(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty())
        {
            let _ = run_quiet("kill", &[&format!("-{signal}"), pid]);
        }
    }
    for item in &d.login_item {
        let name = apple_script_string(item);
        let _ = run_quiet(
            "osascript",
            &[
                "-e",
                &format!("tell application \"System Events\" to delete every login item whose name is \"{name}\""),
            ],
        );
    }
    for script in &d.script {
        run_script(script, &mut warnings);
    }
    for pattern in &d.pkgutil {
        for id in matching_pkgs(pattern) {
            remove_pkg(&id, &mut warnings);
        }
    }
    for path in &d.delete {
        match expand(path) {
            Some(p) => glob(&p).iter().for_each(|m| remove_path(m, &mut warnings)),
            None => warnings.push(format!("skipping unsafe delete path {path}")),
        }
    }
    for path in &d.trash {
        match expand(path) {
            Some(p) => glob(&p).iter().for_each(|m| trash_path(m, &mut warnings)),
            None => warnings.push(format!("skipping unsafe trash path {path}")),
        }
    }
    for path in &d.rmdir {
        if let Some(p) = expand(path) {
            for dir in glob(&p) {
                if !is_protected(&dir) {
                    let _ = std::fs::remove_dir(&dir);
                }
            }
        }
    }
    warnings
}

#[cfg(test)]
mod tests;
