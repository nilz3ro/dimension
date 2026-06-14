//! dimension — Dimension admin CLI
//!
//! Connects to the Dimension gateway REST API using bearer-token auth.
//! All commands are organized as subcommand groups (users, keys, …).

use anyhow::Result;
use clap::{Parser, Subcommand};

mod client;
mod cmd;
mod credentials;
mod output;

use client::GatewayClient;

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "dimension", about = "Dimension admin CLI")]
struct Cli {
    /// Gateway base URL
    #[arg(long, env = "DIMENSION_URL", default_value = "http://d2:3000")]
    url: String,

    /// Admin API token (reads from ~/.dimension/credentials when omitted)
    #[arg(long, env = "DIMENSION_TOKEN")]
    token: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate and store API key in ~/.dimension/credentials
    Login {
        /// API key (prompted interactively if omitted)
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Build a bundle from a Dockerfile and deploy it
    Build(cmd::build::BuildCmd),
    /// Push a pre-built ext4 rootfs to the gateway
    Push(cmd::push::PushCmd),
    /// Rollback a bundle to its previous version
    Rollback(cmd::rollback::RollbackCmd),
    /// User management commands
    Users {
        #[command(subcommand)]
        action: cmd::users::UsersCmd,
    },
    /// API key management commands
    Keys {
        #[command(subcommand)]
        action: cmd::keys::KeysCmd,
    },
    /// Session management commands
    Sessions {
        #[command(subcommand)]
        action: cmd::sessions::SessionsCmd,
    },
    /// Manage workers
    Workers {
        #[command(subcommand)]
        action: cmd::workers::WorkersCmd,
    },
    /// Manage tasks
    Tasks {
        #[command(subcommand)]
        action: cmd::tasks::TasksCmd,
    },
    /// Manage artifacts
    Artifacts {
        #[command(subcommand)]
        action: cmd::artifacts::ArtifactsCmd,
    },
    /// Manage secrets
    Secrets {
        #[command(subcommand)]
        action: cmd::secrets::SecretsCmd,
    },
    /// Check platform health
    Health {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Bundle management commands
    Bundles {
        #[command(subcommand)]
        action: cmd::bundles::BundlesCmd,
    },
    /// Log streaming commands
    Logs {
        #[command(subcommand)]
        action: cmd::logs::LogsCmd,
    },
    /// Run a bundle invocation (sync/async/persistent)
    Run(cmd::run::RunCmd),
    /// Stop a running invocation
    Stop(cmd::stop::StopCmd),
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // `login` does not require an existing token — handle it before resolving.
    if let Commands::Login { api_key } = cli.command {
        return cmd::login::run(&cli.url, api_key).await;
    }

    // Resolve token: CLI flag / env var → credentials file → error.
    let token = credentials::resolve_token(cli.token)?;
    let client = GatewayClient::new(cli.url, token);

    match cli.command {
        Commands::Login { .. } => unreachable!(),
        Commands::Build(build_cmd) => cmd::build::run(build_cmd, &client).await,
        Commands::Push(push_cmd) => cmd::push::run(push_cmd, &client).await,
        Commands::Rollback(rollback_cmd) => cmd::rollback::run(rollback_cmd, &client).await,
        Commands::Users { action } => cmd::users::run(action, &client).await,
        Commands::Keys { action } => cmd::keys::run(action, &client).await,
        Commands::Sessions { action } => cmd::sessions::run(&client, action).await,
        Commands::Workers { action } => cmd::workers::run(&client, action).await,
        Commands::Tasks { action } => cmd::tasks::run(&client, action).await,
        Commands::Artifacts { action } => cmd::artifacts::run(&client, action).await,
        Commands::Secrets { action } => cmd::secrets::run(&client, action).await,
        Commands::Health { json } => cmd::health::run(&client, json).await,
        Commands::Bundles { action } => cmd::bundles::run(&client, action).await,
        Commands::Logs { action } => cmd::logs::run(action, &client).await,
        Commands::Run(run_cmd) => cmd::run::run(run_cmd, &client).await,
        Commands::Stop(stop_cmd) => cmd::stop::run(stop_cmd, &client).await,
    }
}
