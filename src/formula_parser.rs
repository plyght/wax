use crate::error::{Result, WaxError};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BuildSystem {
    Autotools,
    CMake,
    Meson,
    Make,
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
    pub runtime_dependencies: Vec<String>,
    pub build_dependencies: Vec<String>,
    pub build_system: BuildSystem,
    pub install_commands: Vec<String>,
    pub configure_args: Vec<String>,
}

pub struct FormulaParser;

static RE_FIELD: OnceLock<Regex> = OnceLock::new();
static RE_DEPENDS: OnceLock<Regex> = OnceLock::new();
static RE_SYSTEM: OnceLock<Regex> = OnceLock::new();
static RE_VERSION: OnceLock<Regex> = OnceLock::new();

impl FormulaParser {
    #[instrument(skip(ruby_content))]
    pub fn parse_ruby_formula(name: &str, ruby_content: &str) -> Result<ParsedFormula> {
        debug!("Parsing Ruby formula: {}", name);

        let url = Self::extract_field(ruby_content, "url")?;
        let sha256 = Self::extract_field(ruby_content, "sha256")?;
        let desc = Self::extract_field(ruby_content, "desc").ok();
        let homepage = Self::extract_field(ruby_content, "homepage").ok();
        let license = Self::extract_field(ruby_content, "license").ok();

        let version = Self::extract_version_from_url(&url);

        let runtime_dependencies = Self::extract_dependencies(ruby_content, false);
        let build_dependencies = Self::extract_dependencies(ruby_content, true);

        let install_block = Self::extract_install_block(ruby_content)?;
        let build_system = Self::detect_build_system(&install_block);
        let configure_args = Self::extract_configure_args(&install_block);
        let install_commands = Self::extract_install_commands(&install_block);

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
            runtime_dependencies,
            build_dependencies,
            build_system,
            install_commands,
            configure_args,
        })
    }

    fn extract_field(content: &str, field: &str) -> Result<String> {
        let re = RE_FIELD.get_or_init(|| {
            Regex::new(r#"(?m)^\s*(?P<field>url|sha256|desc|homepage|license)\s+"(?P<value>[^"]+)"#).unwrap()
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
            Regex::new(r#"(?m)^\s*depends_on\s+"(?P<dep>[^"]+)"(?:\s*=>\s*:(?P<type>\w+))?"#).unwrap()
        });

        let mut deps = Vec::new();
        for cap in re.captures_iter(content) {
            let is_build = cap.name("type").map(|m| m.as_str() == "build").unwrap_or(false);
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
                    } else if trimmed.ends_with(" do") || trimmed.contains(" {") || (trimmed.starts_with("def ") && !trimmed.starts_with("def install")) {
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

    fn detect_build_system(install_block: &str) -> BuildSystem {
        if install_block.contains("./configure") || install_block.contains("./bootstrap") {
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

    fn extract_configure_args(install_block: &str) -> Vec<String> {
        let re = Regex::new(r#""(?P<arg>--[a-z0-9\-_=#{}/]+)""#).unwrap();
        let mut args = Vec::new();

        for cap in re.captures_iter(install_block) {
            let arg = &cap["arg"];
            if !arg.contains("#{") { // Skip dynamic args for now as we can't easily resolve them
                args.push(arg.to_string());
            }
        }

        args
    }

    fn extract_install_commands(install_block: &str) -> Vec<String> {
        let re = RE_SYSTEM.get_or_init(|| {
            Regex::new(r#"system\s+"(?P<cmd>[^"]+)""#).unwrap()
        });

        let mut commands = Vec::new();
        for cap in re.captures_iter(install_block) {
            commands.push(cap["cmd"].to_string());
        }
        commands
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

        let client = reqwest::Client::new();
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

        let client = reqwest::Client::new();
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
        let expected = "#!/bin/bash\n      exec '/Applications/Firefox.app/Contents/MacOS/firefox' \"$@\"";
        assert_eq!(FormulaParser::extract_shimscript(ruby).unwrap().trim(), expected);
    }
}
