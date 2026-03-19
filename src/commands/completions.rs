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
    let home = std::env::var("HOME")
        .map_err(|_| WaxError::InstallError("$HOME not set".to_string()))?;

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
            println!(
                "\nAdd to your ~/.zshrc if not already present:\n  {}",
                style("fpath=(~/.zsh/completions $fpath)").dim()
            );
            println!("Then run: {}", style("exec zsh").dim());
        }
        Shell::Bash => {
            println!("Completions will load automatically in new shells.");
        }
        Shell::Fish => {
            println!("Completions will load automatically in new shells.");
        }
        _ => {}
    }

    Ok(())
}
