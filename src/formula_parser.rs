use crate::error::{Result, WaxError};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

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
        let pattern = format!(r#"{} ""#, field);
        if let Some(start_idx) = content.find(&pattern) {
            let start = start_idx + pattern.len();
            if let Some(end_idx) = content[start..].find('"') {
                return Ok(content[start..start + end_idx].to_string());
            }
        }

        Err(WaxError::ParseError(format!(
            "Field '{}' not found in formula",
            field
        )))
    }

    fn extract_version_from_url(url: &str) -> String {
        if let Some(filename) = url.split('/').next_back() {
            if let Some(version_part) = filename
                .trim_end_matches(".tar.gz")
                .trim_end_matches(".tar.bz2")
                .trim_end_matches(".tar.xz")
                .trim_end_matches(".zip")
                .rsplit('-')
                .next()
            {
                if version_part.chars().next().is_some_and(|c| c.is_numeric()) {
                    return version_part.to_string();
                }
            }
        }
        "unknown".to_string()
    }

    fn extract_dependencies(content: &str, build_only: bool) -> Vec<String> {
        let mut deps = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("depends_on") {
                if build_only && !trimmed.contains("=> :build") {
                    continue;
                }
                if !build_only && trimmed.contains("=> :build") {
                    continue;
                }

                if let Some(dep_start) = trimmed.find('"') {
                    if let Some(dep_end) = trimmed[dep_start + 1..].find('"') {
                        let dep = trimmed[dep_start + 1..dep_start + 1 + dep_end].to_string();
                        deps.push(dep);
                    }
                }
            }
        }

        deps
    }

    fn extract_install_block(content: &str) -> Result<String> {
        let def_install = "def install";
        if let Some(start_idx) = content.find(def_install) {
            let mut depth = 0;
            let mut in_block = false;
            let mut block = String::new();

            for line in content[start_idx..].lines() {
                if line.trim().starts_with("def install") {
                    in_block = true;
                    depth = 1;
                    continue;
                }

                if in_block {
                    if line.trim().starts_with("def ") {
                        break;
                    }
                    if line.trim() == "end" {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    if line.contains(" do") || line.contains("{") {
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
        let mut args = Vec::new();

        for line in install_block.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('"') && (trimmed.contains("--") || trimmed.contains("=")) {
                let arg = trimmed
                    .trim_start_matches('"')
                    .trim_end_matches(',')
                    .trim_end_matches('"')
                    .trim()
                    .to_string();
                if !arg.is_empty() && !arg.contains("#{") {
                    args.push(arg);
                }
            }
        }

        args
    }

    fn extract_install_commands(install_block: &str) -> Vec<String> {
        let mut commands = Vec::new();

        for line in install_block.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("system ") {
                let cmd_start = trimmed.find('"').unwrap_or(0);
                let cmd_end = trimmed[cmd_start + 1..].find('"').unwrap_or(0);
                if cmd_end > 0 {
                    let command = trimmed[cmd_start + 1..cmd_start + 1 + cmd_end].to_string();
                    commands.push(command);
                }
            }
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
}
