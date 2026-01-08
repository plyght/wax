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

pub fn create_progress_bar(total: u64, prefix: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    let style = ProgressStyle::default_bar()
        .template("{prefix:.bold} {bar:40.cyan/blue} {pos}/{len} {msg}")
        .unwrap()
        .progress_chars("█▓▒░ ");
    pb.set_style(style);
    pb.set_prefix(prefix.to_string());
    pb
}

pub fn print_success(message: &str) {
    let term = Term::stdout();
    let _ = term.write_line(&format!("{} {}", style("✓").green().bold(), message));
}

pub fn print_error(title: &str, reason: &str, suggestions: &[&str]) {
    let term = Term::stderr();
    let _ = term.write_line(&format!("{} {}", style("Error:").red().bold(), title));
    let _ = term.write_line("");
    let _ = term.write_line(&format!("{} {}", style("Reason:").yellow(), reason));

    if !suggestions.is_empty() {
        let _ = term.write_line("");
        let _ = term.write_line(&format!("{}", style("Suggestions:").cyan()));
        for suggestion in suggestions {
            let _ = term.write_line(&format!("  - {}", suggestion));
        }
    }
}

pub fn print_info(message: &str) {
    let term = Term::stdout();
    let _ = term.write_line(&format!("{} {}", style("ℹ").blue().bold(), message));
}

pub fn print_table_header(columns: &[&str]) {
    let term = Term::stdout();
    let header = columns
        .iter()
        .map(|c| style(c).bold().to_string())
        .collect::<Vec<_>>()
        .join("  ");
    let _ = term.write_line(&header);
    let _ = term.write_line(&"─".repeat(80));
}

pub fn print_table_row(columns: &[String]) {
    let term = Term::stdout();
    let row = columns.join("  ");
    let _ = term.write_line(&row);
}
