//! Per-harness concurrency pool (contracts-m5.md C4b, plan D5/D6). Each
//! registered harness id gets a FIFO-fair `tokio::sync::Semaphore`; a
//! rate-limit report installs an exponential backoff (base 500ms, cap 30s)
//! that the next `acquire` waits out before taking a permit, reset on the
//! next successful acquire.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Clone, Copy)]
struct BackoffState {
    attempts: u32,
    deadline: Option<Instant>,
}

pub struct HarnessPool {
    limits: HashMap<String, Arc<Semaphore>>,
    backoff: Mutex<HashMap<String, BackoffState>>,
}

/// An acquired slot. Dropping it returns the slot to the pool. Unregistered
/// harnesses have no semaphore, so their permits are unlimited (`None`).
pub struct PoolPermit {
    _permit: Option<OwnedSemaphorePermit>,
}

impl HarnessPool {
    pub fn new(limits: HashMap<String, usize>) -> Arc<Self> {
        let limits = limits
            .into_iter()
            .map(|(id, n)| (id, Arc::new(Semaphore::new(n))))
            .collect();
        Arc::new(Self {
            limits,
            backoff: Mutex::new(HashMap::new()),
        })
    }

    /// Contract default limits: `claude-code=2`.
    pub fn with_defaults() -> Arc<Self> {
        let mut limits = HashMap::new();
        limits.insert("claude-code".to_string(), 2);
        Self::new(limits)
    }

    pub async fn acquire(self: &Arc<Self>, harness: &str) -> PoolPermit {
        let deadline = {
            let backoff = self.backoff.lock().expect("backoff mutex poisoned");
            backoff.get(harness).and_then(|s| s.deadline)
        };
        if let Some(deadline) = deadline {
            tokio::time::sleep_until(deadline).await;
        }

        let permit = match self.limits.get(harness) {
            Some(sem) => Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .expect("pool semaphore is never closed"),
            ),
            None => None,
        };

        {
            let mut backoff = self.backoff.lock().expect("backoff mutex poisoned");
            if let Some(state) = backoff.get_mut(harness) {
                state.attempts = 0;
                state.deadline = None;
            }
        }

        PoolPermit { _permit: permit }
    }

    /// Records a rate-limit hit for `harness`: the next `acquire` waits
    /// `min(500ms * 2^attempts, 30s)` before proceeding, and `attempts`
    /// increases for each consecutive report until a successful `acquire`
    /// resets it.
    pub fn report_rate_limit(&self, harness: &str) {
        let mut backoff = self.backoff.lock().expect("backoff mutex poisoned");
        let state = backoff.entry(harness.to_string()).or_default();

        let exponent = state.attempts.min(6); // 500ms * 2^6 = 32s already exceeds the 30s cap
        let delay_ms = BACKOFF_BASE.as_millis() as u64 * (1u64 << exponent);
        let delay = Duration::from_millis(delay_ms).min(BACKOFF_CAP);

        state.deadline = Some(Instant::now() + delay);
        state.attempts = state.attempts.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    fn pool_with(harness: &str, limit: usize) -> Arc<HarnessPool> {
        let mut limits = HashMap::new();
        limits.insert(harness.to_string(), limit);
        HarnessPool::new(limits)
    }

    #[tokio::test(start_paused = true)]
    async fn second_acquire_waits_for_first_permit_to_drop() {
        let pool = pool_with("claude-code", 1);

        let first = pool.acquire("claude-code").await;

        let pool2 = pool.clone();
        let second_task = tokio::spawn(async move { pool2.acquire("claude-code").await });

        // Give the spawned task a chance to run and block on the semaphore.
        tokio::task::yield_now().await;
        assert!(!second_task.is_finished(), "second acquire should still be waiting");

        drop(first);
        let second = tokio::time::timeout(StdDuration::from_secs(1), second_task)
            .await
            .expect("second acquire should complete once the first permit drops")
            .expect("task did not panic");
        drop(second);
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_are_released_in_fifo_order() {
        let pool = pool_with("claude-code", 1);
        let order = Arc::new(Mutex::new(Vec::<u32>::new()));

        let held = pool.acquire("claude-code").await;

        let mut tasks = Vec::new();
        for id in [1u32, 2u32] {
            let pool = pool.clone();
            let order = order.clone();
            tasks.push(tokio::spawn(async move {
                let permit = pool.acquire("claude-code").await;
                order.lock().unwrap().push(id);
                permit
            }));
            // Ensure task `id` actually starts waiting before spawning the next one.
            tokio::task::yield_now().await;
        }

        drop(held);
        let first = tasks.remove(0).await.unwrap();
        // First waiter must be granted before the second can proceed.
        assert_eq!(*order.lock().unwrap(), vec![1]);
        drop(first);
        let _second = tasks.remove(0).await.unwrap();
        assert_eq!(*order.lock().unwrap(), vec![1, 2]);
    }

    // Real time (not `start_paused`) — deliberately, since this test only
    // needs to distinguish "resolves basically instantly" from "blocks
    // forever", and mixing a paused clock's auto-advance heuristics with a
    // `join_all` of several futures proved unreliable to reason about.
    #[tokio::test]
    async fn unregistered_harness_is_unlimited() {
        let pool = HarnessPool::new(HashMap::new());

        // Gather 5 concurrent acquires for an id with no configured limit.
        // If `acquire` ever gated an unregistered harness on a real
        // semaphore, some of these would block forever (nothing ever drops
        // an earlier permit); the generous real-time bound turns that into
        // a deterministic assertion failure instead of hanging the test
        // run, while adding ~0ms when (correctly) nothing blocks at all.
        let permits = tokio::time::timeout(
            StdDuration::from_millis(500),
            futures::future::join_all((0..5).map(|_| pool.acquire("unregistered"))),
        )
        .await
        .expect("unregistered harness must not gate concurrent acquires");

        assert_eq!(permits.len(), 5);
        drop(permits);
    }

    #[tokio::test(start_paused = true)]
    async fn report_rate_limit_delays_acquire_exponentially_then_resets_on_success() {
        let pool = pool_with("claude-code", 1);

        pool.report_rate_limit("claude-code");
        let task = tokio::spawn({
            let pool = pool.clone();
            async move { pool.acquire("claude-code").await }
        });

        tokio::time::advance(StdDuration::from_millis(400)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "acquire should not complete before the 500ms base delay");

        tokio::time::advance(StdDuration::from_millis(200)).await;
        let permit = task.await.expect("task did not panic");
        drop(permit);

        // Two consecutive reports (no successful acquire in between) double the delay to 1s.
        pool.report_rate_limit("claude-code");
        pool.report_rate_limit("claude-code");
        let task = tokio::spawn({
            let pool = pool.clone();
            async move { pool.acquire("claude-code").await }
        });

        tokio::time::advance(StdDuration::from_millis(900)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "second consecutive report should back off to ~1s");

        tokio::time::advance(StdDuration::from_millis(200)).await;
        let permit = task.await.expect("task did not panic");

        // A successful acquire resets the backoff — the next report starts back at 500ms.
        drop(permit);
        pool.report_rate_limit("claude-code");
        let task = tokio::spawn({
            let pool = pool.clone();
            async move { pool.acquire("claude-code").await }
        });
        tokio::time::advance(StdDuration::from_millis(600)).await;
        let permit = tokio::time::timeout(StdDuration::from_secs(1), task)
            .await
            .expect("acquire should complete promptly after reset")
            .expect("task did not panic");
        drop(permit);
    }
}
