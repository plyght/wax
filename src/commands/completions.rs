use crate::error::{Result, WaxError};
use clap::CommandFactory;
use clap_complete::{generate, Shell};
use std::io;
use std::path::PathBuf;

use crate::Cli;

pub fn completions(shell: Option<Shell>, print: bool) -> Result<()> {
    let shell = shell.unwrap_or_else(detect_shell);

    if print {
        // --print: dump to stdout for manual piping
        let mut cmd = Cli::command();
        generate(shell, &mut cmd, "wax", &mut io::stdout());
        Ok(())
    } else {
        // Default: auto-detect shell and install completions
        install_completions(shell)
    }
}

fn detect_shell() -> Shell {
    if let Ok(shell_path) = std::env::var("SHELL") {
        let shell_name = shell_path.rsplit('/').next().unwrap_or("");
        match shell_name {
            "zsh" => return Shell::Zsh,
            "bash" => return Shell::Bash,
            "fish" => return Shell::Fish,
            "elvish" => return Shell::Elvish,
            _ => {}
        }
    }
    Shell::Zsh
}

fn install_completions(shell: Shell) -> Result<()> {
    let home =
        std::env::var("HOME").map_err(|_| WaxError::InstallError("$HOME not set".to_string()))?;

    let (dest, content) = match shell {
        Shell::Zsh => {
            let dir = PathBuf::from(&home).join(".zsh/completions");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("_wax");
            let mut buf = Vec::new();
            let mut cmd = Cli::command();
            generate(Shell::Zsh, &mut cmd, "wax", &mut buf);
            (path, buf)
        }
        Shell::Bash => {
            let dir = PathBuf::from(&home).join(".local/share/bash-completion/completions");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("wax");
            let mut buf = Vec::new();
            let mut cmd = Cli::command();
            generate(Shell::Bash, &mut cmd, "wax", &mut buf);
            (path, buf)
        }
        Shell::Fish => {
            let dir = PathBuf::from(&home).join(".config/fish/completions");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("wax.fish");
            let mut buf = Vec::new();
            let mut cmd = Cli::command();
            generate(Shell::Fish, &mut cmd, "wax", &mut buf);
            (path, buf)
        }
        _ => {
            return Err(WaxError::InstallError(format!(
                "Auto-install not supported for {:?}. Use `wax completions {:?}` and redirect manually.",
                shell, shell
            )));
        }
    };

    std::fs::write(&dest, &content)?;

    use console::style;
    println!(
        "{} completions installed to {}",
        style("✓").green(),
        style(dest.display()).cyan()
    );

    match shell {
        Shell::Zsh => {
            let zshrc = PathBuf::from(&home).join(".zshrc");
            let fpath_line = "fpath=(~/.zsh/completions $fpath)";

            let already_configured = std::fs::read_to_string(&zshrc)
                .map(|content| content.contains(fpath_line))
                .unwrap_or(false);

            if already_configured {
                println!(
                    "completions are ready — run {} to activate",
                    style("exec zsh").cyan()
                );
            } else {
                let prompt = inquire::Confirm::new("Add fpath to ~/.zshrc?")
                    .with_default(true)
                    .prompt();

                match prompt {
                    Ok(true) => {
                        let mut zshrc_content = std::fs::read_to_string(&zshrc).unwrap_or_default();
                        // Insert before compinit if present, otherwise append near the top
                        if let Some(pos) = zshrc_content.find("autoload -Uz compinit") {
                            zshrc_content.insert_str(pos, &format!("{}\n", fpath_line));
                        } else if let Some(pos) = zshrc_content.find("compinit") {
                            zshrc_content.insert_str(pos, &format!("{}\n", fpath_line));
                        } else {
                            // Append with a newline
                            if !zshrc_content.ends_with('\n') {
                                zshrc_content.push('\n');
                            }
                            zshrc_content.push_str(fpath_line);
                            zshrc_content.push('\n');
                        }
                        std::fs::write(&zshrc, &zshrc_content)?;
                        println!("{} added fpath to ~/.zshrc", style("✓").green());
                        println!("\nrun {} to activate completions", style("exec zsh").cyan());
                    }
                    Ok(false) => {
                        println!(
                            "\nadd this to your ~/.zshrc manually:\n  {}",
                            style(fpath_line).dim()
                        );
                    }
                    Err(_) => {
                        println!(
                            "\nadd this to your ~/.zshrc manually:\n  {}",
                            style(fpath_line).dim()
                        );
                    }
                }
            }
        }
        Shell::Bash => {
            println!("completions will load automatically in new shells.");
        }
        Shell::Fish => {
            println!("completions will load automatically in new shells.");
        }
        _ => {}
    }

    Ok(())
}
