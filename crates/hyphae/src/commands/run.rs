use anyhow::{Context, Result};

use hyphae_core::net::LanAllow;
use hyphae_core::orchestrator::{ProgressFn, RunRequest};
use hyphae_core::orchestrator::Orchestrator;
use hyphae_core::registry::storage::default_data_dir;
use hyphae_core::registry::Registry;

use crate::cli::RunArgs;
use crate::output::OutputConfig;

pub async fn execute(args: RunArgs, output: &OutputConfig) -> Result<()> {
    let data_dir = default_data_dir().context("Failed to resolve data directory")?;
    let registry = Registry::open(&data_dir).context("Failed to open registry")?;
    let orchestrator = Orchestrator::new(registry);

    let lan_allow = args
        .lan_allow
        .iter()
        .map(|spec| LanAllow::parse(spec).map_err(anyhow::Error::from))
        .collect::<Result<Vec<_>, _>>()
        .context("invalid --lan-allow entry")?;

    let request = RunRequest {
        reference: args.bundle,
        kernel_path: args.kernel,
        vcpus: args.vcpus,
        memory_mib: args.memory,
        boot_args: args.boot_args,
        enable_network: args.network,
        lan_allow,
        no_nat: args.no_nat,
        jail: args.jail,
        vsock: None,
    };

    if args.dry_run {
        let plan = orchestrator
            .plan_run(request)
            .context("Failed to plan run")?;
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

    let result = orchestrator
        .run(request, progress)
        .await
        .context("Run failed")?;
    output.print_result(&result);

    Ok(())
}
