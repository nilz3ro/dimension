//! Graceful shutdown signal handling and final orphan cleanup.
//!
//! [`shutdown_signal`] waits for SIGTERM or SIGINT, then cancels the
//! shutdown token so axum stops accepting new connections and the drain
//! timeout timer starts.
//!
//! [`run_final_orphan_sweep`] runs a safety-net scan for orphaned VM
//! processes after the server has fully stopped.

use tokio_util::sync::CancellationToken;

use hyphae_core::process::{recover_orphans, runtime_base_dir};

/// Wait for SIGTERM or SIGINT, then cancel the shutdown token.
///
/// This function is passed to `axum::serve().with_graceful_shutdown()`.
/// When the signal fires:
/// 1. `shutdown_token` is cancelled (triggers: stop accepting new connections + start drain timer)
/// 2. Axum stops accepting new TCP connections (clients get "connection refused")
/// 3. Axum waits for all existing response bodies to complete
pub async fn shutdown_signal(shutdown_token: CancellationToken) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }

    shutdown_token.cancel();
}

/// Run a final orphan recovery sweep after the server has stopped.
///
/// This is a safety net: [`VmLifecycleGuard`](crate::orchestration::VmLifecycleGuard)
/// Drop should have killed all VMs when their owning tasks were cancelled,
/// but this catches any edge cases (stuck tasks, process abort, etc.).
pub async fn run_final_orphan_sweep() {
    tracing::info!("running final orphan sweep");
    let base = runtime_base_dir();
    let report = recover_orphans(&base).await;

    if report.killed > 0 || report.stale_dirs > 0 {
        tracing::warn!(
            killed = report.killed,
            stale_dirs = report.stale_dirs,
            "final sweep cleaned up remaining VM resources"
        );
    } else {
        tracing::info!("final sweep: no orphans found");
    }
}
