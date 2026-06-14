use clap::Parser;

mod cli;
mod commands;
mod errors;
mod output;

#[tokio::main]
async fn main() {
    let cli = cli::Cli::parse();
    let output = output::OutputConfig::from_global(&cli.global);

    if let Err(e) = run(cli, &output).await {
        let translated = errors::translate_error(&e);
        if output.json {
            let err_json = serde_json::json!({
                "error": translated,
            });
            output.print_error(
                &serde_json::to_string_pretty(&err_json).unwrap_or_default(),
            );
        } else {
            output.print_error(&translated);
        }
        std::process::exit(1);
    }
}

async fn run(cli: cli::Cli, output: &output::OutputConfig) -> anyhow::Result<()> {
    match cli.command {
        cli::Commands::Build(args) => commands::build::execute(args, output).await,
        cli::Commands::Run(args) => commands::run::execute(args, output).await,
        cli::Commands::Stop(args) => commands::stop::execute(args, output).await,
        cli::Commands::List(args) => commands::list::execute(args, output).await,
        cli::Commands::Inspect(args) => commands::inspect::execute(args, output).await,
        cli::Commands::Check => commands::check::execute(output).await,
    }
}
