use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoBuild {
    pub output: String,
    pub target: String,
    pub subdir: Option<String>,
    pub tags: Option<String>,
}

static OUTPUT_RE: OnceLock<Regex> = OnceLock::new();
static TAGS_RE: OnceLock<Regex> = OnceLock::new();
static QUOTED_RE: OnceLock<Regex> = OnceLock::new();
static CD_RE: OnceLock<Regex> = OnceLock::new();

pub(crate) fn is_go_build(install_block: &str) -> bool {
    install_block.contains(r#"system "go", "build""#)
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains("#{")
        && !path.starts_with('/')
        && !path.split('/').any(|part| part == "..")
}

pub(crate) fn extract(install_block: &str, formula_name: &str) -> Option<GoBuild> {
    let output_re = OUTPUT_RE
        .get_or_init(|| Regex::new(r#"(?:output:\s*|"-o",\s*)bin\s*/\s*"([^"]+)""#).unwrap());
    let tags_re = TAGS_RE.get_or_init(|| Regex::new(r##""-tags",\s*"([^"#]+)""##).unwrap());
    let quoted_re = QUOTED_RE.get_or_init(|| Regex::new(r#""([^"]*)""#).unwrap());
    let cd_re = CD_RE.get_or_init(|| Regex::new(r#"^cd\s+"([^"]+)"\s+do$"#).unwrap());

    let lines: Vec<&str> = install_block.lines().map(str::trim).collect();
    let start = lines.iter().position(|l| is_go_build(l))?;
    let mut statement = String::new();
    for line in &lines[start..] {
        statement.push_str(line);
        statement.push(' ');
        if !line.ends_with(',') && !line.ends_with('\\') {
            break;
        }
    }

    let output = output_re
        .captures(&statement)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| formula_name.to_string());
    let target = quoted_re
        .captures_iter(&statement)
        .map(|c| c[1].to_string())
        .filter(|arg| arg == "." || arg.starts_with("./"))
        .last()
        .unwrap_or_else(|| ".".to_string());
    let subdir = lines[..start]
        .iter()
        .rev()
        .find_map(|l| cd_re.captures(l).map(|c| c[1].to_string()));
    let tags = tags_re.captures(&statement).map(|c| c[1].to_string());

    let subdir = subdir.filter(|dir| safe_relative(dir));
    (safe_relative(&output) && !output.contains('/') && safe_relative(&target)).then_some(GoBuild {
        output,
        target,
        subdir,
        tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_go_args_with_cmd_target() {
        let block = r#"
    ldflags = "-s -w -X main.version=#{version}"
    system "go", "build", *std_go_args(ldflags:), "./cmd/gh"
"#;
        assert_eq!(
            extract(block, "gh"),
            Some(GoBuild {
                output: "gh".into(),
                target: "./cmd/gh".into(),
                subdir: None,
                tags: None,
            })
        );
    }

    #[test]
    fn multi_line_output_flag_inside_cd_block() {
        let block = r#"
    cd "src/tool" do
      system "go", "build",
        "-trimpath",
        "-tags", "sqlite_json1 most",
        "-o", bin/"usql"
    end
"#;
        assert_eq!(
            extract(block, "usql"),
            Some(GoBuild {
                output: "usql".into(),
                target: ".".into(),
                subdir: Some("src/tool".into()),
                tags: Some("sqlite_json1 most".into()),
            })
        );
    }

    #[test]
    fn std_go_args_output_rename_and_unsafe_paths() {
        let block = r#"system "go", "build", *std_go_args(output: bin/"k9s", ldflags:)"#;
        assert_eq!(extract(block, "x").unwrap().output, "k9s");
        let block = r#"system "go", "build", "-o", bin/"../evil", "."#;
        assert_eq!(extract(block, "x"), None);
        let block = r#"
    cd "src/#{$pkg}" do
      system "go", "build", "-o", bin/"usql"
    end
"#;
        assert_eq!(extract(block, "usql").unwrap().subdir, None);
    }
}
