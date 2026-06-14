use anyhow::Result;
use serde::Serialize;
use std::fmt;

use crate::output::OutputConfig;

#[derive(Serialize)]
struct CheckOutput {
    checks: Vec<CheckEntry>,
    all_passed: bool,
}

#[derive(Serialize)]
struct CheckEntry {
    name: String,
    passed: bool,
    message: String,
}

impl fmt::Display for CheckOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for entry in &self.checks {
            writeln!(f, "{}", entry.message)?;
        }
        Ok(())
    }
}

pub async fn execute(output: &OutputConfig) -> Result<()> {
    let checks = hyphae_core::prereq::default_checks();
    let results = hyphae_core::prereq::run_all_checks(&checks);

    let mut any_failed = false;
    let mut entries = Vec::new();

    for result in &results {
        if !output.json {
            output.print_step(&result.message);
        }
        if !result.passed {
            any_failed = true;
        }
        entries.push(CheckEntry {
            name: result.name.clone(),
            passed: result.passed,
            message: result.message.clone(),
        });
    }

    if output.json {
        let check_output = CheckOutput {
            checks: entries,
            all_passed: !any_failed,
        };
        output.print_result(&check_output);
    }

    if any_failed {
        anyhow::bail!("One or more prerequisite checks failed");
    }

    Ok(())
}
