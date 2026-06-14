use std::fmt;

use anyhow::{Context, Result};
use serde::Serialize;

use hyphae_core::orchestrator::ProgressFn;
use hyphae_core::orchestrator::Orchestrator;
use hyphae_core::registry::storage::default_data_dir;
use hyphae_core::registry::Registry;

use crate::cli::StopArgs;
use crate::output::OutputConfig;

#[derive(Serialize)]
struct StopOutput {
    vm_id: String,
    status: String,
}

impl fmt::Display for StopOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Stopped VM {}", self.vm_id)
    }
}

pub async fn execute(args: StopArgs, output: &OutputConfig) -> Result<()> {
    let data_dir = default_data_dir().context("Failed to resolve data directory")?;
    let registry = Registry::open(&data_dir).context("Failed to open registry")?;
    let orchestrator = Orchestrator::new(registry);

    if args.dry_run {
        let plan = orchestrator
            .plan_stop(&args.vm_id)
            .context("Failed to plan stop")?;
        output.print_result(&plan);
        return Ok(());
    }

    let progress: Option<ProgressFn> = if !output.quiet && !output.json {
        Some(Box::new(|msg: &str| {
            println!("{msg}");
        }))
    } else {
        None
    };

    orchestrator
        .stop(&args.vm_id, progress)
        .await
        .context("Stop failed")?;

    let stop_output = StopOutput {
        vm_id: args.vm_id,
        status: "stopped".to_string(),
    };
    output.print_result(&stop_output);

    Ok(())
}
