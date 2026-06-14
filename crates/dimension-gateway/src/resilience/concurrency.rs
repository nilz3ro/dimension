//! Hot-reloadable concurrency controller.
//!
//! Wraps a [`tokio::sync::Semaphore`] with dynamic limit adjustment
//! via [`ConcurrencyController::set_limit`]. The semaphore is shared
//! with the concurrency limit middleware, which calls
//! [`try_acquire_owned`](tokio::sync::Semaphore::try_acquire_owned) to
//! enforce the limit with immediate 503 rejection.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Manages a dynamically resizable concurrency limit.
///
/// The underlying semaphore's effective capacity can be changed at runtime
/// without restarting the server. Active requests keep their permits
/// regardless of limit changes -- decreasing the limit takes effect
/// gradually as in-flight requests complete and their permits are released.
///
/// # Usage
///
/// ```rust
/// use dimension_gateway::resilience::ConcurrencyController;
///
/// let controller = ConcurrencyController::new(200);
/// assert_eq!(controller.limit(), 200);
///
/// // Share the semaphore with middleware
/// let semaphore = controller.semaphore();
///
/// // Adjust limit at runtime (e.g., from admin endpoint)
/// controller.set_limit(300);
/// assert_eq!(controller.limit(), 300);
/// ```
pub struct ConcurrencyController {
    semaphore: Arc<Semaphore>,
    /// Tracks the logical configured limit, independent of the number
    /// of currently available permits (which fluctuates as requests
    /// acquire and release permits).
    current_limit: AtomicUsize,
}

impl ConcurrencyController {
    /// Create a new controller with the given initial concurrency limit.
    pub fn new(initial_limit: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(initial_limit)),
            current_limit: AtomicUsize::new(initial_limit),
        }
    }

    /// Return a clone of the underlying semaphore for use in middleware.
    ///
    /// The middleware passes this `Arc<Semaphore>` as state to
    /// [`axum::middleware::from_fn_with_state`] and calls
    /// `try_acquire_owned()` on each request.
    pub fn semaphore(&self) -> Arc<Semaphore> {
        self.semaphore.clone()
    }

    /// Return the current configured limit.
    ///
    /// This is the logical maximum, not the number of currently
    /// available permits. Use [`available`](Self::available) for that.
    pub fn limit(&self) -> usize {
        self.current_limit.load(Ordering::Relaxed)
    }

    /// Return the number of currently available (unacquired) permits.
    pub fn available(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Return the number of currently active (acquired) permits.
    ///
    /// This is `limit() - available()`, representing the number of
    /// in-flight requests holding a permit.
    pub fn active(&self) -> usize {
        self.limit().saturating_sub(self.available())
    }

    /// Change the concurrency limit at runtime.
    ///
    /// - **Increasing:** Adds permits immediately. Waiters (if any) can
    ///   proceed, and new requests see the higher capacity.
    /// - **Decreasing:** Calls [`Semaphore::forget_permits`] to remove
    ///   available permits. If fewer permits are available than the
    ///   decrease amount, only the available ones are forgotten. The
    ///   effective limit reduction takes full effect as in-flight
    ///   requests complete and their permits are released (forgotten
    ///   rather than returned to the pool).
    /// - **Equal:** No-op.
    pub fn set_limit(&self, new_limit: usize) {
        let old_limit = self.current_limit.swap(new_limit, Ordering::Relaxed);

        if new_limit > old_limit {
            let delta = new_limit - old_limit;
            self.semaphore.add_permits(delta);
            tracing::info!(
                old_limit,
                new_limit,
                added = delta,
                "concurrency limit increased"
            );
        } else if new_limit < old_limit {
            let delta = old_limit - new_limit;
            let actually_forgotten = self.semaphore.forget_permits(delta);
            tracing::info!(
                old_limit,
                new_limit,
                requested_reduction = delta,
                actually_forgotten,
                "concurrency limit decreased"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_with_correct_limit() {
        let controller = ConcurrencyController::new(10);
        assert_eq!(controller.limit(), 10);
        assert_eq!(controller.available(), 10);
    }

    #[test]
    fn test_set_limit_increase() {
        let controller = ConcurrencyController::new(10);
        controller.set_limit(15);
        assert_eq!(controller.limit(), 15);
        assert_eq!(controller.available(), 15);
    }

    #[test]
    fn test_set_limit_decrease() {
        let controller = ConcurrencyController::new(10);
        controller.set_limit(5);
        assert_eq!(controller.limit(), 5);
        assert_eq!(controller.available(), 5);
    }

    #[test]
    fn test_set_limit_equal_is_noop() {
        let controller = ConcurrencyController::new(10);
        controller.set_limit(10);
        assert_eq!(controller.limit(), 10);
        assert_eq!(controller.available(), 10);
    }

    #[test]
    fn test_active_count() {
        let controller = ConcurrencyController::new(10);

        // Acquire 3 permits
        let sem = controller.semaphore();
        let _p1 = sem.clone().try_acquire_owned().unwrap();
        let _p2 = sem.clone().try_acquire_owned().unwrap();
        let _p3 = sem.clone().try_acquire_owned().unwrap();

        assert_eq!(controller.active(), 3);
        assert_eq!(controller.available(), 7);
    }

    #[test]
    fn test_decrease_with_held_permits() {
        let controller = ConcurrencyController::new(10);

        // Acquire 8 permits, leaving 2 available
        let sem = controller.semaphore();
        let _permits: Vec<_> = (0..8)
            .map(|_| sem.clone().try_acquire_owned().unwrap())
            .collect();

        // Decrease to 5 -- only 2 available permits can be forgotten
        controller.set_limit(5);
        assert_eq!(controller.limit(), 5);
        // 2 available permits were forgotten; 8 are still held
        // available = 0 (2 were forgotten out of the requested 5 reduction)
        assert_eq!(controller.available(), 0);
    }

    #[test]
    fn test_permits_released_after_drop() {
        let controller = ConcurrencyController::new(2);

        let sem = controller.semaphore();
        let p1 = sem.clone().try_acquire_owned().unwrap();
        let _p2 = sem.clone().try_acquire_owned().unwrap();

        assert_eq!(controller.available(), 0);

        // Drop one permit
        drop(p1);
        assert_eq!(controller.available(), 1);
        assert_eq!(controller.active(), 1);
    }
}
