use console::{style, Term};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub fn create_spinner(message: &str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message(message.to_string());
    spinner
}

pub fn print_success(message: &str) {
    let term = Term::stdout();
    let _ = term.write_line(&format!("{} {}", style("✓").green().bold(), message));
}

pub fn print_info(message: &str) {
    let term = Term::stdout();
    let _ = term.write_line(&format!("{} {}", style("ℹ").blue().bold(), message));
}
