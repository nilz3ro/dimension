use serde::Serialize;
use std::fmt::Display;

pub struct OutputConfig {
    pub json: bool,
    pub quiet: bool,
}

impl OutputConfig {
    pub fn from_global(opts: &crate::cli::GlobalOpts) -> Self {
        Self {
            json: opts.json,
            quiet: opts.quiet,
        }
    }

    /// Print a final result. In JSON mode, serializes to pretty JSON.
    /// In text mode, uses Display impl.
    pub fn print_result<T: Display + Serialize>(&self, value: &T) {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_default()
            );
        } else {
            println!("{value}");
        }
    }

    /// Print a progress step. Suppressed by --quiet and --json.
    pub fn print_step(&self, message: &str) {
        if !self.quiet && !self.json {
            println!("{message}");
        }
    }

    /// Print an error to stderr. Always shown regardless of flags.
    pub fn print_error(&self, error: &str) {
        eprintln!("{error}");
    }
}
