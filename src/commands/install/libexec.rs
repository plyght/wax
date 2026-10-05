use crate::error::{Result, WaxError};
use crate::formula_parser::FormulaParser;
use regex::Regex;
use std::path::{Component, Path};
use std::sync::OnceLock;

#[derive(Debug, PartialEq)]
pub(super) enum Step {
    Libexec {
        source: String,
        optional: bool,
    },
    ExecScript {
        name: String,
    },
    Symlink {
        target: String,
        in_libexec: bool,
        link: String,
    },
}

static LIBEXEC_RE: OnceLock<Regex> = OnceLock::new();
static EXEC_RE: OnceLock<Regex> = OnceLock::new();
static SYMLINK_RE: OnceLock<Regex> = OnceLock::new();
static QUOTED_RE: OnceLock<Regex> = OnceLock::new();

pub(super) fn parse(install_block: &str, version: &str) -> Vec<Step> {
    let libexec_re = LIBEXEC_RE.get_or_init(|| {
        Regex::new(r#"^libexec\.install\s+(.+?)(\s+if\s+File\.exist\?.*)?$"#).unwrap()
    });
    let exec_re = EXEC_RE.get_or_init(|| {
        Regex::new(r#"^bin\.write_exec_script\s*\(?\s*libexec\s*/\s*"([^"]+)"\s*\)?$"#).unwrap()
    });
    let symlink_re = SYMLINK_RE.get_or_init(|| {
        Regex::new(
            r#"^bin\.install_symlink\s*\(?\s*(libexec\s*/\s*)?"([^"]+)"(?:\s*=>\s*"([^"]+)")?\s*\)?$"#,
        )
        .unwrap()
    });
    let quoted_re = QUOTED_RE.get_or_init(|| Regex::new(r#""([^"]+)""#).unwrap());
    let expand = |s: &str| FormulaParser::substitute_ruby_interpolations(s, version, None, None);

    let mut steps = Vec::new();
    for line in install_block.lines().map(str::trim) {
        if let Some(cap) = libexec_re.captures(line) {
            let optional = cap.get(2).is_some();
            let args = &cap[1];
            let args = args
                .strip_prefix("Dir[")
                .and_then(|rest| rest.strip_suffix(']'))
                .unwrap_or(args);
            for quoted in quoted_re.captures_iter(args) {
                steps.push(Step::Libexec {
                    source: expand(&quoted[1]),
                    optional,
                });
            }
        } else if let Some(cap) = exec_re.captures(line) {
            steps.push(Step::ExecScript {
                name: expand(&cap[1]),
            });
        } else if let Some(cap) = symlink_re.captures(line) {
            let target = expand(&cap[2]);
            let link = cap.get(3).map(|m| expand(m.as_str())).unwrap_or_else(|| {
                target
                    .rsplit('/')
                    .next()
                    .unwrap_or(target.as_str())
                    .to_string()
            });
            steps.push(Step::Symlink {
                target,
                in_libexec: cap.get(1).is_some(),
                link,
            });
        }
    }
    steps
}

fn relative(path: &str) -> Result<&Path> {
    let p = Path::new(path);
    if p.components().all(|c| matches!(c, Component::Normal(_))) && !path.is_empty() {
        Ok(p)
    } else {
        Err(WaxError::BuildError(format!(
            "Unsafe install path '{path}'"
        )))
    }
}

fn single(path: &str) -> Result<&Path> {
    let p = relative(path)?;
    if p.components().count() == 1 {
        Ok(p)
    } else {
        Err(WaxError::BuildError(format!("Unsafe bin name '{path}'")))
    }
}

fn copy_into(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::symlink_metadata(src)?.is_dir() {
        crate::ui::copy_dir_all(src, dst)
    } else {
        std::fs::copy(src, dst)?;
        Ok(())
    }
}

fn glob_matches(root: &Path, pattern: &str) -> Result<Vec<std::path::PathBuf>> {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return Ok(vec![root.join(relative(pattern)?)]);
    };
    single(&pattern.replace('*', "x"))?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(prefix) && name.ends_with(suffix) {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

pub(super) fn apply(steps: &[Step], src_dir: &Path, stage: &Path, keg: &Path) -> Result<usize> {
    let libexec = stage.join("libexec");
    let bin = stage.join("bin");
    let mut bins = 0;
    for step in steps {
        match step {
            Step::Libexec { source, optional } => {
                let matches = glob_matches(src_dir, source)?;
                let present: Vec<_> = matches.into_iter().filter(|p| p.exists()).collect();
                if present.is_empty() && !optional {
                    return Err(WaxError::BuildError(format!(
                        "libexec.install target not found: {source}"
                    )));
                }
                for path in present {
                    copy_into(&path, &libexec.join(path.file_name().unwrap()))?;
                }
            }
            Step::ExecScript { name } => {
                let name = single(name)?;
                std::fs::create_dir_all(&bin)?;
                let script = bin.join(name);
                std::fs::write(
                    &script,
                    format!(
                        "#!/bin/bash\nexec \"{}\" \"$@\"\n",
                        keg.join("libexec").join(name).display()
                    ),
                )?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
                }
                bins += 1;
            }
            Step::Symlink {
                target,
                in_libexec,
                link,
            } => {
                let link = single(link)?;
                let target = relative(target)?;
                let target = if *in_libexec {
                    Path::new("../libexec").join(target)
                } else {
                    target.to_path_buf()
                };
                std::fs::create_dir_all(&bin)?;
                let link_path = bin.join(link);
                if std::fs::symlink_metadata(&link_path).is_ok() {
                    std::fs::remove_file(&link_path)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &link_path)?;
                #[cfg(not(unix))]
                copy_into(&bin.join(&target), &link_path)?;
                bins += 1;
            }
        }
    }
    Ok(bins)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEXBAR: &str = r#"
    libexec.install "CodexBarCLI"
    libexec.install "VERSION"
    libexec.install "CodexBar_CodexBarCore.bundle" if File.exist?("CodexBar_CodexBarCore.bundle")
    bin.write_exec_script libexec/"CodexBarCLI"
    bin.install_symlink "CodexBarCLI" => "codexbar"
"#;

    #[test]
    fn parses_libexec_exec_script_and_symlink_steps() {
        assert_eq!(
            parse(CODEXBAR, "1.0"),
            vec![
                Step::Libexec {
                    source: "CodexBarCLI".into(),
                    optional: false
                },
                Step::Libexec {
                    source: "VERSION".into(),
                    optional: false
                },
                Step::Libexec {
                    source: "CodexBar_CodexBarCore.bundle".into(),
                    optional: true
                },
                Step::ExecScript {
                    name: "CodexBarCLI".into()
                },
                Step::Symlink {
                    target: "CodexBarCLI".into(),
                    in_libexec: false,
                    link: "codexbar".into()
                },
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn applies_steps_into_stage_with_keg_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("CodexBarCLI"), b"bin").unwrap();
        std::fs::write(src.join("VERSION"), b"1.0").unwrap();
        let stage = tmp.path().join("stage");
        let keg = Path::new("/opt/homebrew/Cellar/codexbar/1.0");

        let bins = apply(&parse(CODEXBAR, "1.0"), &src, &stage, keg).unwrap();

        assert_eq!(bins, 2);
        assert_eq!(
            std::fs::read(stage.join("libexec/VERSION")).unwrap(),
            b"1.0"
        );
        assert!(std::fs::read_to_string(stage.join("bin/CodexBarCLI"))
            .unwrap()
            .contains("/opt/homebrew/Cellar/codexbar/1.0/libexec/CodexBarCLI"));
        assert_eq!(
            std::fs::read_link(stage.join("bin/codexbar")).unwrap(),
            Path::new("CodexBarCLI")
        );
    }

    #[test]
    fn rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let steps = parse(r#"bin.install_symlink libexec/"x" => "../evil""#, "1");
        assert!(apply(&steps, tmp.path(), tmp.path(), tmp.path()).is_err());
        let steps = parse(r#"libexec.install "../../etc""#, "1");
        assert!(apply(&steps, tmp.path(), tmp.path(), tmp.path()).is_err());
    }
}
