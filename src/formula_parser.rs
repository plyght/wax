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

impl FormulaParser {
    #[instrument(skip(ruby_content))]
    pub fn parse_ruby_formula(name: &str, ruby_content: &str) -> Result<ParsedFormula> {
        debug!("Parsing Ruby formula: {}", name);

        let url = Self::extract_field(ruby_content, "url")?;
        let sha256 = Self::extract_field(ruby_content, "sha256")?;
        let desc = Self::extract_field(ruby_content, "desc").ok();
        let homepage = Self::extract_field(ruby_content, "homepage").ok();
        let license = Self::extract_field(ruby_content, "license").ok();
        let head_url = Self::extract_head_url(ruby_content);

        // Prefer an explicit `version "x.y.z"` field; fall back to parsing from URL.
        let version = Self::extract_field(ruby_content, "version")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| Self::extract_version_from_url(&url));

        let runtime_dependencies = Self::extract_dependencies(ruby_content, false);
        let build_dependencies = Self::extract_dependencies(ruby_content, true);

        let install_block = Self::extract_install_block(ruby_content)?;
        let build_system = Self::detect_build_system(&install_block);
        let configure_args = Self::extract_configure_args(&install_block);
        let install_commands = Self::extract_install_commands(&install_block);
        let bin_installs = Self::extract_bin_installs(&install_block);

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
            Regex::new(r"(?:[-_/]|^)(?P<version>\d+\.\d+(?:\.\d+)*(?:[_-][a-z\d]+)*)").unwrap()
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
    pub(crate) fn extract_bin_installs(install_block: &str) -> Vec<String> {
        let re = Regex::new(r#"bin\.install\s+"([^"]+)""#).unwrap();
        re.captures_iter(install_block)
            .map(|c| c[1].to_string())
            .collect()
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

        // 1. OS block → preferred arch → whole OS block → fallback arch
        if let Some(os_block) = Self::extract_named_block(content, os_block_key) {
            if let Some(arch_block) = Self::extract_named_block(&os_block, arch_preferred) {
                if let Some(pair) = try_extract(&arch_block) {
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

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| WaxError::ParseError(format!("Failed to create HTTP client: {}", e)))?;
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

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| WaxError::ParseError(format!("Failed to create HTTP client: {}", e)))?;
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
}
