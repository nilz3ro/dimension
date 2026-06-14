//! Resource-aware worker selection for multi-host execution.
//!
//! The scheduler picks workers using round-robin with resource filtering:
//! spread requests across all workers that meet the resource requirements,
//! cycling through them to maximize horizontal scaling.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::worker::registry::{WorkerRegistry, WorkerState};

/// Global round-robin counter. Incremented on each pick.
static ROUND_ROBIN: AtomicUsize = AtomicUsize::new(0);

/// Select the next worker for a request using round-robin with resource filtering.
///
/// # Algorithm
///
/// 1. Filter out draining workers.
/// 2. Filter to workers with `available_memory_mb >= required_memory_mb`
///    and `available_vcpus >= required_vcpus`.
/// 3. Sort qualifying workers by ID for stable ordering.
/// 4. Pick the next worker in round-robin order.
///
/// This spreads requests evenly across all qualifying workers instead of
/// always picking the one with the most available memory.
///
/// Returns `None` if no worker qualifies.
pub fn pick_worker(
    registry: &WorkerRegistry,
    required_memory_mb: u64,
    required_vcpus: u32,
) -> Option<WorkerState> {
    let all = registry.get_all();

    let mut candidates: Vec<WorkerState> = all
        .into_iter()
        .filter(|w| !w.draining)
        .filter(|w| w.available_memory_mb >= required_memory_mb)
        .filter(|w| w.available_vcpus >= required_vcpus)
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Sort by worker_id for stable ordering across calls
    candidates.sort_by_key(|w| w.worker_id);

    let idx = ROUND_ROBIN.fetch_add(1, Ordering::Relaxed) % candidates.len();
    Some(candidates.swap_remove(idx))
}
