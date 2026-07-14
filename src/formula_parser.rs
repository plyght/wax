use crate::api::{Cask, CaskArtifact, CaskDetails};
use crate::error::{Result, WaxError};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tracing::{debug, instrument};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BuildSystem {
    Autotools,
    CMake,
    Meson,
    Make,
    Cargo,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormulaSource {
    pub url: String,
    pub sha256: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedFormula {
    pub name: String,
    pub desc: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub source: FormulaSource,
    /// HEAD git URL, if the formula defines one.
    pub head_url: Option<String>,
    pub runtime_dependencies: Vec<String>,
    pub build_dependencies: Vec<String>,
    pub build_system: BuildSystem,
    pub install_commands: Vec<String>,
    pub configure_args: Vec<String>,
    /// Files to copy to `bin/` via `bin.install "..."` (binary-release formulas).
    pub bin_installs: Vec<String>,
    pub bin_install_targets: Vec<BinInstall>,
    pub share_install_targets: Vec<ShareInstall>,
}

pub struct FormulaParser;

static RE_FIELD: OnceLock<Regex> = OnceLock::new();
static RE_DEPENDS: OnceLock<Regex> = OnceLock::new();
static RE_SYSTEM: OnceLock<Regex> = OnceLock::new();
static RE_VERSION: OnceLock<Regex> = OnceLock::new();
static RE_HEAD: OnceLock<Regex> = OnceLock::new();
static RE_CASK_URL: OnceLock<Regex> = OnceLock::new();
static RE_CASK_SHA: OnceLock<Regex> = OnceLock::new();

/// Linux artifact extracted from a Homebrew cask's `on_linux` block.
#[derive(Debug, Clone)]
pub struct CaskLinuxArtifact {
    /// Download URL for the artifact (.deb, .rpm, .AppImage, etc.)
    pub url: String,
    /// sha256 checksum, or `None` if the cask uses `:no_check`.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinInstall {
    pub source: String,
    pub destination: String,
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareInstall {
    pub source: String,
    /// Relative prefix inside `share/`, e.g. `sketchybar/examples`
    pub dest_prefix: String,
    pub destination: String,
    pub optional: bool,
}

impl FormulaParser {
    #[instrument(skip(ruby_content))]
    pub fn parse_ruby_formula(name: &str, ruby_content: &str) -> Result<ParsedFormula> {
        debug!("Parsing Ruby formula: {}", name);

        let head_url = Self::extract_head_url(ruby_content);
        let platform_source = Self::extract_platform_source(ruby_content);
        let url = Self::extract_field(ruby_content, "url").or_else(|e| {
            if head_url.is_some() || platform_source.is_some() {
                Ok(String::new())
            } else {
                Err(e)
            }
        })?;
        let sha256 = Self::extract_field(ruby_content, "sha256").or_else(|e| {
            if head_url.is_some() || platform_source.is_some() {
                Ok(String::new())
            } else {
                Err(e)
            }
        })?;
        let desc = Self::extract_field(ruby_content, "desc").ok();
        let homepage = Self::extract_field(ruby_content, "homepage").ok();
        let license = Self::extract_field(ruby_content, "license").ok();

        // Prefer an explicit `version "x.y.z"` field; fall back to parsing from URL.
        let version = Self::extract_field(ruby_content, "version")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                if !url.is_empty() {
                    Self::extract_version_from_url(&url)
                } else if let Some((ref platform_url, _)) = platform_source {
                    Self::extract_version_from_url(platform_url)
                } else if head_url.is_some() {
                    "HEAD".to_string()
                } else {
                    "unknown".to_string()
                }
            });

        let (url, sha256) = if url.is_empty() {
            if let Some((platform_url, platform_sha)) = platform_source {
                (platform_url, platform_sha)
            } else {
                (url, sha256)
            }
        } else {
            (url, sha256)
        };

        let runtime_dependencies = Self::extract_dependencies(ruby_content, false);
        let build_dependencies = Self::extract_dependencies(ruby_content, true);

        let install_block = Self::extract_install_block(ruby_content)
            .or_else(|_| Self::extract_define_method_install_block(ruby_content))
            .or_else(|_| {
                Self::extract_platform_install_block(ruby_content).ok_or_else(|| {
                    WaxError::ParseError("Install block not found in formula".to_string())
                })
            })?;
        let build_system = Self::detect_build_system(&install_block);
        let configure_args = Self::extract_configure_args(&install_block);
        let install_commands = Self::extract_install_commands(&install_block);
        let bin_install_targets = Self::extract_bin_install_targets(&install_block);
        let bin_installs = bin_install_targets
            .iter()
            .map(|target| target.source.clone())
            .collect();

        let share_install_targets = Self::extract_share_install_targets(&install_block);

        Ok(ParsedFormula {
            name: name.to_string(),
            desc,
            homepage,
            license,
            source: FormulaSource {
                url,
                sha256,
                version,
            },
            head_url,
            runtime_dependencies,
            build_dependencies,
            build_system,
            install_commands,
            configure_args,
            bin_installs,
            bin_install_targets,
            share_install_targets,
        })
    }

    fn extract_head_url(content: &str) -> Option<String> {
        let re = RE_HEAD.get_or_init(|| Regex::new(r#"(?m)^\s*head\s+"([^"]+)""#).unwrap());
        re.captures(content).map(|c| c[1].to_string())
    }

    fn extract_field(content: &str, field: &str) -> Result<String> {
        let re = RE_FIELD.get_or_init(|| {
            Regex::new(r#"(?m)^\s*(?P<field>url|sha256|desc|homepage|license|version)\s+"(?P<value>[^"]+)"#)
                .unwrap()
        });

        for cap in re.captures_iter(content) {
            if &cap["field"] == field {
                return Ok(cap["value"].to_string());
            }
        }

        Err(WaxError::ParseError(format!(
            "Field '{}' not found in formula",
            field
        )))
    }

    fn extract_version_from_url(url: &str) -> String {
        let re = RE_VERSION.get_or_init(|| {
            Regex::new(r"(?:[-_/]|^)v?(?P<version>\d+\.\d+(?:\.\d+)*(?:[_-][a-z\d]+)*)").unwrap()
        });

        if let Some(filename) = url.split('/').next_back() {
            if let Some(cap) = re.captures(filename) {
                return cap["version"].to_string();
            }
        }
        "unknown".to_string()
    }

    fn extract_dependencies(content: &str, build_only: bool) -> Vec<String> {
        let re = RE_DEPENDS.get_or_init(|| {
            Regex::new(r#"(?m)^\s*depends_on\s+"(?P<dep>[^"]+)"(?:\s*=>\s*:(?P<type>\w+))?"#)
                .unwrap()
        });

        let mut deps = Vec::new();
        for cap in re.captures_iter(content) {
            let is_build = cap
                .name("type")
                .map(|m| m.as_str() == "build")
                .unwrap_or(false);
            if build_only == is_build {
                deps.push(cap["dep"].to_string());
            }
        }
        deps
    }

    fn extract_install_block(content: &str) -> Result<String> {
        let start_marker = "def install";
        if let Some(start_idx) = content.find(start_marker) {
            let mut depth = 0;
            let mut block = String::new();
            let mut started = false;

            for line in content[start_idx..].lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("def install") {
                    started = true;
                    depth = 1;
                    continue;
                }

                if started {
                    if trimmed == "end" {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    } else if Self::opens_ruby_block(trimmed) {
                        depth += 1;
                    }
                    block.push_str(line);
                    block.push('\n');
                }
            }

            if !block.is_empty() {
                return Ok(block);
            }
        }

        Err(WaxError::ParseError(
            "Install block not found in formula".to_string(),
        ))
    }

    fn extract_define_method_install_block(content: &str) -> Result<String> {
        let markers = ["define_method(:install) do", "define_method(:install) {"];
        for marker in markers {
            if let Some(start_idx) = content.find(marker) {
                let block = Self::extract_ruby_block_body(&content[start_idx..], marker)?;
                if !block.is_empty() {
                    return Ok(block);
                }
            }
        }
        Err(WaxError::ParseError(
            "Install block not found in formula".to_string(),
        ))
    }

    fn extract_platform_install_block(content: &str) -> Option<String> {
        let is_arm = std::env::consts::ARCH == "aarch64";
        let os_block_key = if std::env::consts::OS == "macos" {
            "on_macos do"
        } else {
            "on_linux do"
        };
        let arch_preferred = if is_arm { "on_arm do" } else { "on_intel do" };
        let cpu_preferred = if is_arm {
            "if Hardware::CPU.arm?"
        } else {
            "if Hardware::CPU.intel?"
        };

        let os_block = Self::extract_named_block(content, os_block_key)?;
        let search_blocks = [
            Self::extract_named_block(&os_block, arch_preferred),
            Self::extract_hardware_cpu_block(&os_block, cpu_preferred),
            Some(os_block),
        ];
        for block in search_blocks.into_iter().flatten() {
            if let Ok(install) = Self::extract_define_method_install_block(&block) {
                return Some(install);
            }
            if let Ok(install) = Self::extract_install_block(&block) {
                return Some(install);
            }
        }
        None
    }

    fn extract_hardware_cpu_block(content: &str, start_keyword: &str) -> Option<String> {
        Self::extract_named_block(content, start_keyword)
    }

    fn extract_ruby_block_body(content: &str, open_marker: &str) -> Result<String> {
        let mut depth = 0usize;
        let mut block = String::new();
        let mut started = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if !started {
                if trimmed.contains(open_marker) || trimmed.starts_with(open_marker) {
                    started = true;
                    depth = 1;
                }
                continue;
            }

            if trimmed == "end"
                || trimmed.starts_with("end ")
                || trimmed.starts_with("end\t")
                || trimmed.starts_with("end#")
            {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            } else if Self::opens_ruby_block(trimmed) {
                depth += 1;
            }

            block.push_str(line);
            block.push('\n');
        }

        if started && !block.is_empty() {
            Ok(block)
        } else {
            Err(WaxError::ParseError(
                "Install block not found in formula".to_string(),
            ))
        }
    }

    fn opens_ruby_block(trimmed: &str) -> bool {
        trimmed.ends_with(" do")
            || trimmed.contains(" {")
            || trimmed.starts_with("if ")
            || trimmed.starts_with("unless ")
            || trimmed.starts_with("case ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("until ")
            || trimmed.starts_with("for ")
            || trimmed.starts_with("begin")
            || trimmed.starts_with("class ")
            || trimmed.starts_with("module ")
            || (trimmed.starts_with("def ") && !trimmed.starts_with("def install"))
    }

    fn detect_build_system(install_block: &str) -> BuildSystem {
        if install_block.contains("cargo") {
            BuildSystem::Cargo
        } else if install_block.contains("./configure") || install_block.contains("./bootstrap") {
            BuildSystem::Autotools
        } else if install_block.contains("cmake") {
            BuildSystem::CMake
        } else if install_block.contains("meson") {
            BuildSystem::Meson
        } else if install_block.contains(r#"system "make""#) {
            BuildSystem::Make
        } else {
            BuildSystem::Unknown
        }
    }

    /// cmake mode verbs that appear as quoted args in `system "cmake", "--build", ...` calls.
    /// These are NOT configure options and must not be forwarded to the cmake -S/-B step.
    const CMAKE_MODE_VERBS: &'static [&'static str] = &[
        "--build",
        "--install",
        "--open",
        "--preset",
        "--fresh",
        "--list-presets",
        "--workflow",
        "--version",
        "--help",
    ];

    fn extract_configure_args(install_block: &str) -> Vec<String> {
        // Match args in double quotes: "--flag" or "-DFLAG=val"
        let re_quoted =
            Regex::new(r#""(?P<arg>(?:--[a-z0-9\-_=#{}/]+|-D[A-Za-z0-9_=\-#{}/.:+]+))""#).unwrap();
        // Match bare args inside %W[...] or %w[...] word arrays (no quotes)
        let re_word_array = Regex::new(r#"%[Ww]\[(?P<body>[^\]]*)\]"#).unwrap();
        let re_bare_arg =
            Regex::new(r"(?P<arg>(?:--[a-z0-9\-_=]+|-D[A-Za-z0-9_=\-.:+]+))").unwrap();

        let mut args = Vec::new();

        for cap in re_quoted.captures_iter(install_block) {
            let arg = &cap["arg"];
            if !arg.contains("#{") && !Self::CMAKE_MODE_VERBS.contains(&arg) {
                args.push(arg.to_string());
            }
        }

        for cap in re_word_array.captures_iter(install_block) {
            let body = &cap["body"];
            for token in body.split_whitespace() {
                if let Some(m) = re_bare_arg.find(token) {
                    let arg = m.as_str();
                    // Skip tokens containing interpolation (#{...})
                    if !token.contains("#{") {
                        args.push(arg.to_string());
                    }
                }
            }
        }

        args
    }

    fn extract_install_commands(install_block: &str) -> Vec<String> {
        let re = RE_SYSTEM.get_or_init(|| Regex::new(r#"system\s+"(?P<cmd>[^"]+)""#).unwrap());

        let mut commands = Vec::new();
        for cap in re.captures_iter(install_block) {
            commands.push(cap["cmd"].to_string());
        }
        commands
    }

    /// Parse `bin.install "filename"` entries from a formula install block.
    #[cfg(test)]
    pub(crate) fn extract_bin_installs(install_block: &str) -> Vec<String> {
        Self::extract_bin_install_targets(install_block)
            .into_iter()
            .map(|target| target.source)
            .collect()
    }

    fn extract_dir_first_assignments(
        install_block: &str,
    ) -> std::collections::HashMap<String, String> {
        let re = Regex::new(r#"(?m)^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*Dir\["([^"]+)"\]\.first"#)
            .unwrap();
        re.captures_iter(install_block)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .collect()
    }

    pub(crate) fn extract_bin_install_targets(install_block: &str) -> Vec<BinInstall> {
        let re = Regex::new(r#"bin\.install\s+"([^"]+)"(?:\s*=>\s*"([^"]+)")?"#).unwrap();
        let dir_re =
            Regex::new(r#"bin\.install\s+Dir\["([^"]+)"\]\.first(?:\s*=>\s*"([^"]+)")?"#).unwrap();
        let var_re =
            Regex::new(r#"bin\.install\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s*=>\s*"([^"]+)")?"#).unwrap();
        let dir_vars = Self::extract_dir_first_assignments(install_block);
        let mut targets: Vec<BinInstall> = Vec::new();
        for line in install_block.lines() {
            targets.extend(re.captures_iter(line).map(|c| {
                let source = c[1].to_string();
                let destination = c.get(2).map(|m| m.as_str().to_string()).unwrap_or_else(|| {
                    source
                        .rsplit('/')
                        .next()
                        .unwrap_or(source.as_str())
                        .to_string()
                });
                BinInstall {
                    destination,
                    optional: line.contains("if File.exist?"),
                    source,
                }
            }));
            targets.extend(dir_re.captures_iter(line).map(|c| {
                let source = c[1].to_string();
                let destination = c
                    .get(2)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_else(|| source.clone());
                BinInstall {
                    destination,
                    optional: line.contains("if File.exist?"),
                    source,
                }
            }));
            targets.extend(var_re.captures_iter(line).filter_map(|c| {
                let var = c[1].to_string();
                let glob = dir_vars.get(&var)?.clone();
                let destination = c
                    .get(2)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_else(|| var.clone());
                Some(BinInstall {
                    destination,
                    optional: line.contains("if File.exist?"),
                    source: glob,
                })
            }));
        }
        targets
    }

    /// Extract `(pkgshare/"sub").install "file"` targets.
    fn extract_share_install_targets(install_block: &str) -> Vec<ShareInstall> {
        let re = Regex::new(
            r#"(?x)
            \( pkgshare \s* (?: / \s* "([^"]*)" )? \) 
            \.install \s+ "([^"]+)"
        "#,
        )
        .unwrap();
        let mut targets = Vec::new();
        for cap in re.captures_iter(install_block) {
            let sub = cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let source = cap[2].to_string();
            let clean_source = source.trim_end_matches('/');
            let destination = clean_source
                .rsplit('/')
                .next()
                .unwrap_or(clean_source)
                .to_string();
            targets.push(ShareInstall {
                source,
                dest_prefix: sub,
                destination,
                optional: false,
            });
        }
        targets
    }

    /// For formulas with `on_linux`/`on_macos`/`on_arm`/`on_intel` conditional blocks,
    /// extract the (url, sha256) pair appropriate for the current platform.
    /// Returns `None` if no matching block is found.
    pub fn extract_platform_source(content: &str) -> Option<(String, String)> {
        let is_arm = std::env::consts::ARCH == "aarch64";
        let os_block_key = if std::env::consts::OS == "macos" {
            "on_macos do"
        } else {
            "on_linux do"
        };
        let arch_preferred = if is_arm { "on_arm do" } else { "on_intel do" };
        let arch_fallback = if is_arm { "on_intel do" } else { "on_arm do" };

        let try_extract = |block: &str| -> Option<(String, String)> {
            let art = Self::extract_url_sha(block)?;
            Some((art.url, art.sha256?))
        };

        let cpu_preferred = if is_arm {
            "if Hardware::CPU.arm?"
        } else {
            "if Hardware::CPU.intel?"
        };
        let cpu_fallback = if is_arm {
            "if Hardware::CPU.intel?"
        } else {
            "if Hardware::CPU.arm?"
        };

        // 1. OS block → preferred arch → Hardware::CPU → whole OS block → fallback arch
        if let Some(os_block) = Self::extract_named_block(content, os_block_key) {
            if let Some(arch_block) = Self::extract_named_block(&os_block, arch_preferred) {
                if let Some(pair) = try_extract(&arch_block) {
                    return Some(pair);
                }
            }
            if let Some(cpu_block) = Self::extract_hardware_cpu_block(&os_block, cpu_preferred) {
                if let Some(pair) = try_extract(&cpu_block) {
                    return Some(pair);
                }
            }
            // No arch sub-block — use the whole OS block directly.
            if let Some(pair) = try_extract(&os_block) {
                return Some(pair);
            }
            if let Some(arch_block) = Self::extract_named_block(&os_block, arch_fallback) {
                if let Some(pair) = try_extract(&arch_block) {
                    return Some(pair);
                }
            }
            if let Some(cpu_block) = Self::extract_hardware_cpu_block(&os_block, cpu_fallback) {
                if let Some(pair) = try_extract(&cpu_block) {
                    return Some(pair);
                }
            }
        }

        // 2. Direct arch blocks at top level (no OS wrapper).
        if let Some(arch_block) = Self::extract_named_block(content, arch_preferred) {
            if let Some(pair) = try_extract(&arch_block) {
                return Some(pair);
            }
        }

        None
    }

    /// Parse the Linux-specific artifact from a Homebrew cask `.rb` file.
    ///
    /// Handles:
    /// - `on_intel do` / `on_arm do` named blocks (newer cask style)
    /// - `on_linux do` blocks with `if Hardware::CPU.intel?` / `if Hardware::CPU.arm?`
    /// - `on_linux do` blocks with a single URL (no CPU branching)
    pub fn parse_cask_linux_artifact(content: &str) -> Option<CaskLinuxArtifact> {
        let is_arm = std::env::consts::ARCH == "aarch64";

        // 1. Try architecture-specific named blocks (on_arm do / on_intel do).
        let preferred = if is_arm { "on_arm do" } else { "on_intel do" };
        let fallback = if is_arm { "on_intel do" } else { "on_arm do" };

        if let Some(block) = Self::extract_named_block(content, preferred) {
            if let Some(art) = Self::extract_url_sha(&block) {
                return Some(art);
            }
        }

        // 2. Try on_linux block (with optional CPU conditional inside).
        if let Some(linux_block) = Self::extract_named_block(content, "on_linux do") {
            let cpu_key = if is_arm {
                "if Hardware::CPU.arm?"
            } else {
                "if Hardware::CPU.intel?"
            };
            // Try CPU-specific sub-block first, then fall back to whole on_linux block.
            let search_in = Self::extract_named_block(&linux_block, cpu_key)
                .unwrap_or_else(|| linux_block.clone());
            if let Some(art) = Self::extract_url_sha(&search_in) {
                return Some(art);
            }
        }

        // 3. Fallback arch block.
        if let Some(block) = Self::extract_named_block(content, fallback) {
            if let Some(art) = Self::extract_url_sha(&block) {
                return Some(art);
            }
        }

        None
    }

    /// Extract a named Ruby block (e.g. `on_linux do ... end`) from content.
    /// Returns the block body (lines between the opening and matching `end`).
    fn extract_named_block(content: &str, start_keyword: &str) -> Option<String> {
        let mut found = false;
        let mut depth = 0usize;
        let mut block = String::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if !found {
                if trimmed.starts_with(start_keyword) {
                    found = true;
                    depth = 1;
                }
                continue;
            }

            let is_end = trimmed == "end"
                || trimmed.starts_with("end ")
                || trimmed.starts_with("end\t")
                || trimmed.starts_with("end#");

            if is_end {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            } else if Self::opens_ruby_block(trimmed) {
                depth += 1;
            }

            block.push_str(line);
            block.push('\n');
        }

        if found && !block.is_empty() {
            Some(block)
        } else {
            None
        }
    }

    /// Extract the first `url` + `sha256` pair from a block of Ruby cask content.
    fn extract_url_sha(block: &str) -> Option<CaskLinuxArtifact> {
        let re_url = RE_CASK_URL.get_or_init(|| Regex::new(r#"(?m)^\s*url\s+"([^"]+)""#).unwrap());
        let re_sha = RE_CASK_SHA
            .get_or_init(|| Regex::new(r#"(?m)^\s*sha256\s+(?:"([^"]+)"|:no_check)"#).unwrap());

        let url = re_url.captures(block).map(|c| c[1].to_string())?;
        let sha256 = re_sha
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        Some(CaskLinuxArtifact { url, sha256 })
    }

    /// True when `.rb` is a Homebrew cask (`cask "token" do`), not a formula class.
    pub fn is_homebrew_cask_rb(content: &str) -> bool {
        content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .is_some_and(|line| line.starts_with("cask "))
    }

    fn extract_cask_string_field(content: &str, field: &str) -> Option<String> {
        let pattern = format!(r#"(?m)^\s*{field}\s+(?:"([^"]+)"|'([^']+)')"#);
        let re = Regex::new(&pattern).ok()?;
        re.captures(content).and_then(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str().to_string())
        })
    }

    fn extract_cask_version(content: &str) -> String {
        if let Some(v) = Self::extract_cask_string_field(content, "version") {
            return v;
        }
        if let Ok(url) = Self::extract_field(content, "url") {
            return Self::extract_version_from_url(&url);
        }
        "unknown".to_string()
    }

    fn extract_cask_sha256(content: &str) -> String {
        let re = RE_CASK_SHA
            .get_or_init(|| Regex::new(r#"(?m)^\s*sha256\s+(?:"([^"]+)"|:no_check)"#).unwrap());
        re.captures(content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    }

    fn extract_cask_artifacts(content: &str) -> Vec<CaskArtifact> {
        let mut artifacts = Vec::new();
        let re_app = Regex::new(r#"(?m)^\s*app\s+"([^"]+)""#).ok();
        if let Some(re) = re_app {
            for cap in re.captures_iter(content) {
                artifacts.push(CaskArtifact::App {
                    app: vec![serde_json::Value::String(cap[1].to_string())],
                });
            }
        }
        let re_pkg = Regex::new(r#"(?m)^\s*pkg\s+"([^"]+)""#).ok();
        if let Some(re) = re_pkg {
            for cap in re.captures_iter(content) {
                artifacts.push(CaskArtifact::Pkg {
                    pkg: vec![serde_json::Value::String(cap[1].to_string())],
                });
            }
        }
        let re_binary =
            Regex::new(r#"(?m)^\s*binary\s+"([^"]+)"(?:\s*,\s*\{\s*target:\s*"([^"]+)"\s*\})?"#)
                .ok();
        if let Some(re) = re_binary {
            for cap in re.captures_iter(content) {
                let source = cap[1].to_string();
                let mut binary = vec![serde_json::Value::String(source)];
                if let Some(target) = cap.get(2) {
                    let mut obj = serde_json::Map::new();
                    obj.insert(
                        "target".to_string(),
                        serde_json::Value::String(target.as_str().to_string()),
                    );
                    binary.push(serde_json::Value::Object(obj));
                }
                artifacts.push(CaskArtifact::Binary { binary });
            }
        }
        artifacts
    }

    /// Parse a Homebrew cask `.rb` into summary metadata for catalog lookup.
    pub fn parse_ruby_cask(token: &str, tap_full_name: &str, ruby_content: &str) -> Result<Cask> {
        let version = Self::extract_cask_version(ruby_content);
        let desc = Self::extract_cask_string_field(ruby_content, "desc");
        let homepage =
            Self::extract_cask_string_field(ruby_content, "homepage").unwrap_or_default();
        let display_name = Self::extract_cask_string_field(ruby_content, "name")
            .map(|n| vec![n])
            .unwrap_or_else(|| vec![token.to_string()]);
        Ok(Cask {
            token: token.to_string(),
            full_token: format!("{}/{}", tap_full_name, token),
            name: display_name,
            desc,
            homepage,
            version,
            deprecated: false,
            disabled: false,
            rb_path: None,
        })
    }

    /// Parse a Homebrew cask `.rb` into installable details (tap-local, no API).
    pub fn parse_ruby_cask_details(token: &str, ruby_content: &str) -> Result<CaskDetails> {
        let re_url = RE_CASK_URL.get_or_init(|| Regex::new(r#"(?m)^\s*url\s+"([^"]+)""#).unwrap());
        let url = re_url
            .captures(ruby_content)
            .map(|c| c[1].to_string())
            .ok_or_else(|| WaxError::ParseError(format!("url not found in cask {}", token)))?;
        let version = Self::extract_cask_version(ruby_content);
        let desc = Self::extract_cask_string_field(ruby_content, "desc");
        let homepage =
            Self::extract_cask_string_field(ruby_content, "homepage").unwrap_or_default();
        let display_name = Self::extract_cask_string_field(ruby_content, "name")
            .map(|n| vec![n])
            .unwrap_or_else(|| vec![token.to_string()]);
        let sha256 = Self::extract_cask_sha256(ruby_content);
        let artifacts = Self::extract_cask_artifacts(ruby_content);
        Ok(CaskDetails {
            token: token.to_string(),
            name: display_name,
            desc,
            homepage,
            version,
            url,
            sha256,
            artifacts: if artifacts.is_empty() {
                None
            } else {
                Some(artifacts)
            },
        })
    }

    pub async fn fetch_formula_rb(formula_name: &str) -> Result<String> {
        let first_letter = formula_name
            .chars()
            .next()
            .ok_or_else(|| WaxError::ParseError("Empty formula name".to_string()))?
            .to_lowercase();

        let url = format!(
            "https://raw.githubusercontent.com/Homebrew/homebrew-core/master/Formula/{}/{}.rb",
            first_letter, formula_name
        );

        debug!("Fetching formula from: {}", url);

        let client = crate::http_client::default_client();
        let response = client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(WaxError::ParseError(format!(
                "Failed to fetch formula: HTTP {}",
                response.status()
            )));
        }

        let content = response.text().await?;
        Ok(content)
    }

    pub async fn fetch_cask_rb(cask_name: &str) -> Result<String> {
        let first_letter = cask_name
            .chars()
            .next()
            .ok_or_else(|| WaxError::ParseError("Empty cask name".to_string()))?
            .to_lowercase();

        let url = format!(
            "https://raw.githubusercontent.com/Homebrew/homebrew-cask/master/Casks/{}/{}.rb",
            first_letter, cask_name
        );

        debug!("Fetching cask from: {}", url);

        let client = crate::http_client::default_client();
        let response = client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(WaxError::ParseError(format!(
                "Failed to fetch cask: HTTP {}",
                response.status()
            )));
        }

        let content = response.text().await?;
        Ok(content)
    }

    pub fn extract_shimscript(content: &str) -> Option<String> {
        let re = Regex::new(r"(?m)File\.write\s+(?:shimscript|\w+),\s*<<~([A-Z_]+)\n").ok()?;

        if let Some(cap) = re.captures(content) {
            let delim = &cap[1];
            let start = cap.get(0).unwrap().end();
            let rest = &content[start..];

            // Find the delimiter on a line by itself (ignoring leading whitespace)
            let end_re_str = format!(r"(?m)^\s*{}$", delim);
            if let Ok(end_re) = Regex::new(&end_re_str) {
                if let Some(end_match) = end_re.find(rest) {
                    let mut script = rest[..end_match.start()].to_string();

                    // Basic interpolations
                    script = script.replace("#{appdir}", "/Applications");
                    return Some(script);
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_version_from_url() {
        let url = "https://github.com/example/tree/archive/refs/tags/2.2.1.tar.gz";
        let version = FormulaParser::extract_version_from_url(url);
        assert_eq!(version, "2.2.1");

        let url_v = "https://github.com/example/tree/archive/refs/tags/v0.20.5.tar.gz";
        let version_v = FormulaParser::extract_version_from_url(url_v);
        assert_eq!(version_v, "0.20.5");
    }

    #[test]
    fn test_detect_build_system() {
        let autotools = r#"system "./configure", "--prefix=#{prefix}""#;
        assert_eq!(
            FormulaParser::detect_build_system(autotools),
            BuildSystem::Autotools
        );

        let cmake = r#"system "cmake", "-S", ".", "-B", "build""#;
        assert_eq!(
            FormulaParser::detect_build_system(cmake),
            BuildSystem::CMake
        );

        let make = r#"system "make", "install""#;
        assert_eq!(FormulaParser::detect_build_system(make), BuildSystem::Make);

        let cargo = r#"system "cargo", "install", *std_cargo_args(path: "brush-shell")"#;
        assert_eq!(
            FormulaParser::detect_build_system(cargo),
            BuildSystem::Cargo
        );
    }

    #[test]
    fn test_extract_shimscript() {
        let ruby = r#"
  preflight do
    File.write shimscript, <<~EOS
      #!/bin/bash
      exec '#{appdir}/Firefox.app/Contents/MacOS/firefox' "$@"
    EOS
  end
        "#;
        let expected =
            "#!/bin/bash\n      exec '/Applications/Firefox.app/Contents/MacOS/firefox' \"$@\"";
        assert_eq!(
            FormulaParser::extract_shimscript(ruby).unwrap().trim(),
            expected
        );
    }

    #[test]
    fn test_extract_install_block_with_nested_if() {
        let formula = r#"
class Fastfetch < Formula
  def install
    args = ["-DENABLE_SYSTEM_YYJSON=ON"]
    if HOMEBREW_PREFIX.to_s != HOMEBREW_DEFAULT_PREFIX
      args << "-DCUSTOM_PCRE2=ON"
    end
    system "cmake", "-S", ".", "-B", "build", *args, *std_cmake_args
    system "cmake", "--build", "build"
  end
end
        "#;

        let block = FormulaParser::extract_install_block(formula).unwrap();
        assert!(
            block.contains(r#"system "cmake", "-S", ".", "-B", "build", *args, *std_cmake_args"#)
        );
        assert!(block.contains(r#"system "cmake", "--build", "build""#));
    }

    #[test]
    fn test_extract_cmake_define_args() {
        let install_block = r#"
system "cmake", "-S", ".", "-B", "build", "-DBUILD_FLASHFETCH=OFF", "-DENABLE_SYSTEM_YYJSON=ON", *std_cmake_args
        "#;

        let args = FormulaParser::extract_configure_args(install_block);
        assert!(args.contains(&"-DBUILD_FLASHFETCH=OFF".to_string()));
        assert!(args.contains(&"-DENABLE_SYSTEM_YYJSON=ON".to_string()));
    }

    #[test]
    fn test_extract_cmake_define_args_from_word_array() {
        // Fastfetch-style: args defined in %W[...] then splatted into system call
        let install_block = r#"
    args = %W[
      -DCMAKE_INSTALL_SYSCONFDIR=#{etc}
      -DBUILD_FLASHFETCH=OFF
      -DENABLE_SYSTEM_YYJSON=ON
    ]
    system "cmake", "-S", ".", "-B", "build", *args, *std_cmake_args
        "#;

        let args = FormulaParser::extract_configure_args(install_block);
        // Interpolated arg must be skipped
        assert!(!args.iter().any(|a| a.contains("#{") || a.contains("etc}")));
        // Static -D args must be captured
        assert!(args.contains(&"-DBUILD_FLASHFETCH=OFF".to_string()));
        assert!(args.contains(&"-DENABLE_SYSTEM_YYJSON=ON".to_string()));
    }

    #[test]
    fn test_cmake_mode_verbs_not_captured_as_configure_args() {
        // --build and --install are cmake mode verbs, not configure flags.
        // They must NOT appear in configure_args or they break the cmake -S/-B step.
        let install_block = r#"
    system "cmake", "-S", ".", "-B", "build", "-DFOO=ON", *std_cmake_args
    system "cmake", "--build", "build"
    system "cmake", "--install", "build"
        "#;

        let args = FormulaParser::extract_configure_args(install_block);
        assert!(
            !args.contains(&"--build".to_string()),
            "--build must not be a configure arg"
        );
        assert!(
            !args.contains(&"--install".to_string()),
            "--install must not be a configure arg"
        );
        assert!(args.contains(&"-DFOO=ON".to_string()));
    }

    #[test]
    fn is_homebrew_cask_rb_distinguishes_formula_class() {
        let formula = r#"
class Sketchybar < Formula
  url "https://example.com/x.tar.gz"
end
"#;
        let cask = r#"cask "aerospace" do
  version "1"
end"#;
        assert!(!FormulaParser::is_homebrew_cask_rb(formula));
        assert!(FormulaParser::is_homebrew_cask_rb(cask));
    }

    #[test]
    fn test_parse_ruby_cask_tap_aerospace_shape() {
        let rb = r#"
cask "aerospace" do
  version '0.21.2-Beta'
  sha256 "abc"
  url "https://github.com/nikitabobko/AeroSpace/releases/download/v#{version}/AeroSpace-v#{version}.zip"
  name "AeroSpace"
  desc "tiling wm"
  homepage "https://github.com/nikitabobko/AeroSpace"
  app "AeroSpace-v#{version}/AeroSpace.app"
  binary "AeroSpace-v#{version}/bin/aerospace"
end
"#;
        let summary = FormulaParser::parse_ruby_cask("aerospace", "nikitabobko/tap", rb).unwrap();
        assert_eq!(summary.full_token, "nikitabobko/tap/aerospace");
        assert_eq!(summary.version, "0.21.2-Beta");
        let details = FormulaParser::parse_ruby_cask_details("aerospace", rb).unwrap();
        assert!(details.url.contains("github.com"));
        assert_eq!(details.sha256, "abc");
        assert!(details
            .artifacts
            .as_ref()
            .unwrap()
            .iter()
            .any(|a| matches!(a, crate::api::CaskArtifact::App { .. })));
    }

    #[test]
    fn test_parse_cask_linux_artifact_on_linux_block() {
        let cask = r#"
cask "myapp" do
  version "1.2.3"

  on_macos do
    url "https://example.com/myapp-1.2.3.dmg"
    sha256 "aabbcc"
  end

  on_linux do
    url "https://example.com/myapp-1.2.3-linux.deb"
    sha256 "ddeeff"
  end
end
"#;
        let art = FormulaParser::parse_cask_linux_artifact(cask).unwrap();
        assert_eq!(art.url, "https://example.com/myapp-1.2.3-linux.deb");
        assert_eq!(art.sha256.as_deref(), Some("ddeeff"));
    }

    #[test]
    fn test_parse_cask_linux_artifact_on_intel_arm_blocks() {
        let cask = r#"
cask "myapp" do
  on_intel do
    url "https://example.com/myapp-amd64.deb"
    sha256 "intel_sha"
  end
  on_arm do
    url "https://example.com/myapp-arm64.deb"
    sha256 "arm_sha"
  end
end
"#;
        let art = FormulaParser::parse_cask_linux_artifact(cask).unwrap();
        // On x86_64 we expect the intel artifact; on aarch64 the arm one.
        if std::env::consts::ARCH == "aarch64" {
            assert_eq!(art.url, "https://example.com/myapp-arm64.deb");
        } else {
            assert_eq!(art.url, "https://example.com/myapp-amd64.deb");
        }
    }

    #[test]
    fn test_parse_cask_linux_artifact_no_check_sha() {
        let cask = r#"
cask "myapp" do
  on_linux do
    url "https://example.com/myapp.AppImage"
    sha256 :no_check
  end
end
"#;
        let art = FormulaParser::parse_cask_linux_artifact(cask).unwrap();
        assert_eq!(art.url, "https://example.com/myapp.AppImage");
        assert!(art.sha256.is_none(), "sha256 should be None for :no_check");
    }

    #[test]
    fn test_parse_cask_linux_artifact_returns_none_for_macos_only() {
        let cask = r#"
cask "macos-only-app" do
  url "https://example.com/app.dmg"
  sha256 "abc123"
end
"#;
        assert!(FormulaParser::parse_cask_linux_artifact(cask).is_none());
    }

    #[test]
    fn extract_bin_installs_finds_quoted_filenames() {
        let install_block = r#"
    bin.install "poke-around"
    bin.install "poke-around-bridge.js"
    bin.install "menubar_linux.py" if File.exist?("menubar_linux.py")
"#;
        let bins = FormulaParser::extract_bin_installs(install_block);
        assert_eq!(
            bins,
            vec!["poke-around", "poke-around-bridge.js", "menubar_linux.py"]
        );
    }

    #[test]
    fn extract_bin_install_targets_finds_renames() {
        let install_block = r#"
    bin.install "amp-darwin-arm64" => "amp"
"#;
        let bins = FormulaParser::extract_bin_install_targets(install_block);
        assert_eq!(bins[0].source, "amp-darwin-arm64");
        assert_eq!(bins[0].destination, "amp");
    }

    #[test]
    fn extract_bin_install_targets_finds_dir_first_renames() {
        let install_block = r#"
    bin.install Dir["amp-*"].first => "amp"
"#;
        let bins = FormulaParser::extract_bin_install_targets(install_block);
        assert_eq!(bins[0].source, "amp-*");
        assert_eq!(bins[0].destination, "amp");
    }

    #[test]
    fn extract_bin_install_targets_resolves_dir_first_variable() {
        let install_block = r#"
    binary = Dir["folk-around-*"].first || "folk-around"
    bin.install binary => "folk-around"
"#;
        let bins = FormulaParser::extract_bin_install_targets(install_block);
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0].source, "folk-around-*");
        assert_eq!(bins[0].destination, "folk-around");
    }

    #[test]
    fn extract_bin_installs_empty_for_build_formulas() {
        let install_block = r#"
    system "./configure", "--prefix=#{prefix}"
    system "make", "install"
"#;
        assert!(FormulaParser::extract_bin_installs(install_block).is_empty());
    }

    #[test]
    fn extract_platform_source_linux_intel() {
        let formula = r#"
class MyTool < Formula
  on_macos do
    on_arm do
      url "https://example.com/mytool-macos-arm64.tar.gz"
      sha256 "aaaa"
    end
    on_intel do
      url "https://example.com/mytool-macos-x86_64.tar.gz"
      sha256 "bbbb"
    end
  end
  on_linux do
    on_intel do
      url "https://example.com/mytool-linux-x86_64.tar.gz"
      sha256 "cccc"
    end
  end
end
"#;
        let result = FormulaParser::extract_platform_source(formula);
        // On Linux x86_64 we expect the linux-intel URL.
        if std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64" {
            let (url, sha) = result.unwrap();
            assert_eq!(url, "https://example.com/mytool-linux-x86_64.tar.gz");
            assert_eq!(sha, "cccc");
        }
    }

    #[test]
    fn extract_platform_source_returns_none_without_matching_block() {
        let formula = r#"
class MacOnly < Formula
  on_macos do
    url "https://example.com/maconly.dmg"
    sha256 "aaaa"
  end
end
"#;
        if std::env::consts::OS == "linux" {
            assert!(FormulaParser::extract_platform_source(formula).is_none());
        }
    }

    #[test]
    fn parse_goreleaser_define_method_install_formula() {
        let formula = r#"
class Ketch < Formula
  desc "Fast web search and scrape CLI for agents"
  homepage "https://github.com/1broseidon/ketch"
  version "0.11.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/1broseidon/ketch/releases/download/v0.11.0/ketch_0.11.0_darwin_arm64.tar.gz"
      sha256 "8cc6039ac4911e3cee326a0fc9d3db43fb8529f7dc8e3e942674f8e7a09f56ed"
      define_method(:install) do
        bin.install "ketch"
      end
    end
  end
end
"#;
        let parsed = FormulaParser::parse_ruby_formula("ketch", formula).unwrap();
        assert_eq!(parsed.bin_installs, vec!["ketch"]);
        assert_eq!(parsed.source.version, "0.11.0");
        if std::env::consts::OS == "macos" && std::env::consts::ARCH == "aarch64" {
            assert!(parsed.source.url.contains("darwin_arm64"));
            assert!(!parsed.source.sha256.is_empty());
        }
    }

    #[test]
    fn parse_head_only_formula() {
        let formula = r#"
class DriftWallpaper < Formula
  desc "Fluid live wallpaper"
  homepage "https://github.com/undivisible/drift-wallpaper"
  version "0.1.0"
  license "MPL-2.0"
  head "https://github.com/undivisible/drift-wallpaper.git", branch: "m"

  depends_on "rust" => :build

  def install
    system "cargo", "build", "--release", "-p", "drift-app", "--locked"
    bin.install "target/release/drift-wallpaper"
  end
end
"#;

        let parsed = FormulaParser::parse_ruby_formula("drift-wallpaper", formula).unwrap();
        assert_eq!(parsed.source.version, "0.1.0");
        assert!(parsed.source.url.is_empty());
        assert!(parsed.source.sha256.is_empty());
        assert_eq!(
            parsed.head_url.as_deref(),
            Some("https://github.com/undivisible/drift-wallpaper.git")
        );
    }

    #[test]
    fn test_parse_ruby_formula_comprehensive() {
        let formula = r#"
class Fastfetch < Formula
  desc "Like neofetch, but much faster"
  homepage "https://github.com/fastfetch-cli/fastfetch"
  license "MIT"
  url "https://github.com/fastfetch-cli/fastfetch/archive/refs/tags/2.11.2.tar.gz"
  sha256 "0f24ce73295b9c512033c46e01766a5035e076735e160eafebbdc86db254bdba"

  depends_on "cmake" => :build
  depends_on "pkg-config" => :build
  depends_on "glib"
  depends_on "vulkan-loader"

  def install
    system "cmake", "-S", ".", "-B", "build", *std_cmake_args
    system "cmake", "--build", "build"
    system "cmake", "--install", "build"
    bash_completion.install share/"bash-completion/completions/fastfetch"
  end
end
        "#;

        let parsed = FormulaParser::parse_ruby_formula("fastfetch", formula).unwrap();

        assert_eq!(parsed.name, "fastfetch");
        assert_eq!(
            parsed.desc.as_deref(),
            Some("Like neofetch, but much faster")
        );
        assert_eq!(
            parsed.homepage.as_deref(),
            Some("https://github.com/fastfetch-cli/fastfetch")
        );
        assert_eq!(parsed.license.as_deref(), Some("MIT"));
        assert_eq!(
            parsed.source.url,
            "https://github.com/fastfetch-cli/fastfetch/archive/refs/tags/2.11.2.tar.gz"
        );
        assert_eq!(
            parsed.source.sha256,
            "0f24ce73295b9c512033c46e01766a5035e076735e160eafebbdc86db254bdba"
        );
        assert_eq!(parsed.source.version, "2.11.2");
        assert!(parsed.head_url.is_none());

        assert_eq!(parsed.build_dependencies, vec!["cmake", "pkg-config"]);
        assert_eq!(parsed.runtime_dependencies, vec!["glib", "vulkan-loader"]);
        assert_eq!(parsed.build_system, BuildSystem::CMake);
    }

    #[test]
    fn test_parse_ruby_formula_no_url_or_head() {
        let formula = r#"
class Fastfetch < Formula
  desc "Like neofetch, but much faster"
  homepage "https://github.com/fastfetch-cli/fastfetch"
  license "MIT"
end
        "#;

        let result = FormulaParser::parse_ruby_formula("fastfetch", formula);
        assert!(
            result.is_err(),
            "Expected error when formula lacks both url and head"
        );
    }
}
