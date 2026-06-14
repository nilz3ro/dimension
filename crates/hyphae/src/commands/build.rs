use anyhow::{Context, Result};

use hyphae_core::orchestrator::{BuildRequest, ProgressFn};
use hyphae_core::orchestrator::Orchestrator;
use hyphae_core::registry::storage::default_data_dir;
use hyphae_core::registry::Registry;

use crate::cli::BuildArgs;
use crate::output::OutputConfig;

pub async fn execute(args: BuildArgs, output: &OutputConfig) -> Result<()> {
    // Validate that either project, --binary, or --docker is provided.
    if args.project.is_none() && args.binary.is_none() && args.docker.is_none() {
        anyhow::bail!(
            "Either a project directory, --binary, or --docker must be provided.\n\
             Usage: hyphae build <PROJECT> or hyphae build --binary <PATH> or hyphae build --docker <IMAGE>"
        );
    }

    let data_dir = default_data_dir().context("Failed to resolve data directory")?;
    let registry = Registry::open(&data_dir).context("Failed to open registry")?;
    let orchestrator = Orchestrator::new(registry);

    let binary_path = match &args.binary {
        Some(b) => Some(
            b.canonicalize()
                .with_context(|| format!("Binary path does not exist: {}", b.display()))?,
        ),
        None => None,
    };

    let project = match &args.project {
        Some(p) => p
            .canonicalize()
            .with_context(|| format!("Project path does not exist: {}", p.display()))?,
        None => std::env::current_dir().context("Failed to resolve current directory")?,
    };

    let request = BuildRequest {
        project_dir: project,
        name: args.name,
        tag: args.tag,
        force: args.force,
        default_vcpus: 2,
        default_memory_mib: 256,
        embed_dimension_agent: args.with_dimension,
        binary_path,
        entrypoint: args.entrypoint,
        docker_image: args.docker,
    };

    if args.dry_run {
        let plan = orchestrator
            .plan_build(request)
            .context("Failed to plan build")?;
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
        .build(request, progress)
        .context("Build failed")?;
    output.print_result(&result);

    Ok(())
}
