//! Output formatting helpers for the dimension CLI.
//!
//! Provides table rendering, JSON output, interactive confirmation prompts,
//! and dry-run notices used consistently across all commands.

use std::io::{self, Write};

use comfy_table::{presets::UTF8_FULL, Table};

/// Print a formatted table to stdout using the UTF8_FULL preset.
///
/// `headers` is a slice of column header names.
/// `rows` is a vector of rows, each row being a vector of cell strings.
pub fn print_table(headers: &[&str], rows: Vec<Vec<String>>) {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(headers.to_vec());
    for row in rows {
        table.add_row(row);
    }
    println!("{table}");
}

/// Serialize `value` to pretty-printed JSON and print to stdout.
pub fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error serializing JSON: {e}"),
    }
}

/// Prompt the user for confirmation at the terminal.
///
/// Prints `"{prompt} [y/N] "`, reads a line from stdin, and returns `true`
/// only when the response is "y" or "yes" (case-insensitive).
pub fn confirm(prompt: &str) -> bool {
    print!("{prompt} [y/N] ");
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    let trimmed = input.trim().to_lowercase();
    trimmed == "y" || trimmed == "yes"
}

/// Print a dry-run notice to stdout instead of executing a destructive action.
///
/// Format: `[dry-run] Would {action} {resource} {id} -- no changes made.`
pub fn dry_run_notice(resource: &str, id: &str, action: &str) {
    println!("[dry-run] Would {action} {resource} {id} -- no changes made.");
}
