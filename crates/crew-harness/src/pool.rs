//! Per-harness concurrency pool (contracts-m5.md C4b, plan D5/D6; adaptive
//! reduction/recovery added by contracts-m8.md F1). Each registered harness
//! id gets a FIFO-fair internal slot pool; a rate-limit report installs an
//! exponential backoff (base 500ms, cap 30s) that the next `acquire` waits
//! out before taking a slot, reset on the next successful acquire. The same
//! report also lowers the harness's effective concurrency limit by one (down
//! to a floor of 1, deduped within a cooldown), which lazily recovers by one
//! per cooldown interval, up to the configured base limit, the next time
//! `acquire` is called.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Seconds between lazy recovery steps: each elapses restores one unit of
/// concurrency (up to the harness's base limit) after a rate-limit report.
pub const RATE_RECOVERY_SECS: u64 = 300;

#[derive(Debug, Default, Clone, Copy)]
struct BackoffState {
    attempts: u32,
    deadline: Option<Instant>,
}

/// Adaptive concurrency state for one registered harness.
struct LimitState {
    base: usize,
    current: usize,
    active: usize,
    last_report: Option<Instant>,
}

struct HarnessLimit {
    state: Mutex<LimitState>,
    notify: Notify,
}

pub struct HarnessPool {
    limits: HashMap<String, Arc<HarnessLimit>>,
    backoff: Mutex<HashMap<String, BackoffState>>,
}

/// An acquired slot. Dropping it returns the slot to the pool. Unregistered
/// harnesses have no limit state, so their permits are unlimited (`None`).
pub struct PoolPermit {
    limit: Option<Arc<HarnessLimit>>,
}

impl Drop for PoolPermit {
    fn drop(&mut self) {
        if let Some(limit) = self.limit.take() {
            {
                let mut state = limit.state.lock().expect("pool limit mutex poisoned");
                state.active = state.active.saturating_sub(1);
            }
            limit.notify.notify_waiters();
        }
    }
}

/// Applies any recovery steps owed since `state.last_report`, given the
/// current time. Lazy: only ever called from the `acquire` path, never from
/// a background task.
fn apply_lazy_recovery(state: &mut LimitState, now: Instant) {
    let last = match state.last_report {
        Some(last) => last,
        None => return,
    };

    if state.current >= state.base {
        state.last_report = None;
        return;
    }

    let elapsed = now.saturating_duration_since(last);
    let steps = elapsed.as_secs() / RATE_RECOVERY_SECS;
    if steps == 0 {
        return;
    }

    let recovered = usize::try_from(steps).unwrap_or(usize::MAX);
    state.current = state.current.saturating_add(recovered).min(state.base);
    state.last_report = if state.current >= state.base {
        None
    } else {
        Some(last + Duration::from_secs(steps * RATE_RECOVERY_SECS))
    };
}

impl HarnessPool {
    pub fn new(limits: HashMap<String, usize>) -> Arc<Self> {
        let limits = limits
            .into_iter()
            .map(|(id, n)| {
                let limit = HarnessLimit {
                    state: Mutex::new(LimitState {
                        base: n,
                        current: n,
                        active: 0,
                        last_report: None,
                    }),
                    notify: Notify::new(),
                };
                (id, Arc::new(limit))
            })
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

        let limit = self.limits.get(harness).cloned();
        if let Some(limit) = &limit {
            loop {
                // Register as a waiter before checking the condition, so a
                // `notify_waiters` that lands between the check and the
                // await can never be missed.
                let notified = limit.notify.notified();
                {
                    let mut state = limit.state.lock().expect("pool limit mutex poisoned");
                    apply_lazy_recovery(&mut state, Instant::now());
                    if state.active < state.current {
                        state.active += 1;
                        break;
                    }
                }
                notified.await;
            }
        }

        {
            let mut backoff = self.backoff.lock().expect("backoff mutex poisoned");
            if let Some(state) = backoff.get_mut(harness) {
                state.attempts = 0;
                state.deadline = None;
            }
        }

        PoolPermit { limit }
    }

    /// Records a rate-limit hit for `harness`: the next `acquire` waits
    /// `min(500ms * 2^attempts, 30s)` before proceeding, and `attempts`
    /// increases for each consecutive report until a successful `acquire`
    /// resets it.
    ///
    /// The same report also lowers `harness`'s effective concurrency limit
    /// by one (floor 1), unless a previous report already did so within the
    /// last `RATE_RECOVERY_SECS` (dedup/idempotent). Unregistered harnesses
    /// have no limit state, so this half is a no-op for them; the backoff
    /// bookkeeping above still applies as before.
    pub fn report_rate_limit(&self, harness: &str) {
        let mut backoff = self.backoff.lock().expect("backoff mutex poisoned");
        let state = backoff.entry(harness.to_string()).or_default();

        let exponent = state.attempts.min(6); // 500ms * 2^6 = 32s already exceeds the 30s cap
        let delay_ms = BACKOFF_BASE.as_millis() as u64 * (1u64 << exponent);
        let delay = Duration::from_millis(delay_ms).min(BACKOFF_CAP);

        state.deadline = Some(Instant::now() + delay);
        state.attempts = state.attempts.saturating_add(1);
        drop(backoff);

        if let Some(limit) = self.limits.get(harness) {
            let mut state = limit.state.lock().expect("pool limit mutex poisoned");
            let now = Instant::now();
            let within_cooldown = match state.last_report {
                Some(last) => now.saturating_duration_since(last) < Duration::from_secs(RATE_RECOVERY_SECS),
                None => false,
            };
            if !within_cooldown {
                state.current = state.current.saturating_sub(1).max(1);
            }
            state.last_report = Some(now);
        }
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

    #[tokio::test(start_paused = true)]
    async fn report_rate_limit_reduces_effective_limit_and_gates_second_acquire() {
        let pool = pool_with("claude-code", 2);

        pool.report_rate_limit("claude-code"); // base 2 -> effective 1
        tokio::time::advance(StdDuration::from_millis(600)).await; // clear the 500ms backoff deadline

        let first = pool.acquire("claude-code").await;

        let pool2 = pool.clone();
        let second_task = tokio::spawn(async move { pool2.acquire("claude-code").await });
        tokio::task::yield_now().await;
        assert!(
            !second_task.is_finished(),
            "one report on a limit-2 harness should reduce the effective limit to 1 and gate the second acquire"
        );

        drop(first);
        let second = tokio::time::timeout(StdDuration::from_secs(1), second_task)
            .await
            .expect("second acquire should complete once the first permit drops")
            .expect("task did not panic");
        drop(second);
    }

    #[tokio::test(start_paused = true)]
    async fn report_rate_limit_floors_effective_limit_at_one() {
        let pool = pool_with("claude-code", 1);

        // Three consecutive reports would drive the limit below 1 without the floor.
        pool.report_rate_limit("claude-code");
        pool.report_rate_limit("claude-code");
        pool.report_rate_limit("claude-code");

        // Clear the compounded backoff deadline (500ms, 1s, 2s -> ~2s total).
        tokio::time::advance(StdDuration::from_millis(2100)).await;

        let permit = tokio::time::timeout(StdDuration::from_secs(1), pool.acquire("claude-code"))
            .await
            .expect("the effective limit must floor at 1, not fall to 0 and hang acquire forever");
        drop(permit);
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_reports_within_cooldown_reduce_the_limit_only_once() {
        let pool = pool_with("claude-code", 3);

        pool.report_rate_limit("claude-code"); // base 3 -> effective 2
        pool.report_rate_limit("claude-code"); // within cooldown — must be a no-op for the limit

        // One full recovery interval after the (deduped) reduction. If the
        // duplicate report had instead reduced the limit twice (3 -> 1),
        // one interval would only restore it to 2, and a third concurrent
        // acquire below would still have to wait.
        tokio::time::advance(StdDuration::from_secs(RATE_RECOVERY_SECS)).await;

        let a = pool.acquire("claude-code").await;
        let b = pool.acquire("claude-code").await;
        let pool2 = pool.clone();
        let third_task = tokio::spawn(async move { pool2.acquire("claude-code").await });
        tokio::task::yield_now().await;
        assert!(
            third_task.is_finished(),
            "duplicate report within cooldown must reduce the limit only once, so one recovery \
             interval should fully restore it to base (3)"
        );
        let c = third_task.await.expect("task did not panic");
        drop(a);
        drop(b);
        drop(c);
    }

    #[tokio::test(start_paused = true)]
    async fn limit_recovers_by_one_per_interval_without_exceeding_base() {
        let pool = pool_with("claude-code", 3);

        pool.report_rate_limit("claude-code"); // base 3 -> effective 2
        tokio::time::advance(StdDuration::from_millis(600)).await; // clear the backoff deadline

        // Before any recovery interval elapses, only 2 concurrent acquires succeed.
        let a = pool.acquire("claude-code").await;
        let b = pool.acquire("claude-code").await;
        let pool2 = pool.clone();
        let third_task = tokio::spawn(async move { pool2.acquire("claude-code").await });
        tokio::task::yield_now().await;
        assert!(!third_task.is_finished(), "reduced limit (3->2) should gate a third concurrent acquire");
        drop(a);
        drop(b);
        let c = tokio::time::timeout(StdDuration::from_secs(1), third_task)
            .await
            .expect("third acquire should complete once a permit drops")
            .expect("task did not panic");
        drop(c);

        // One recovery interval after the report restores exactly +1 (2 -> 3 = base) — not beyond.
        tokio::time::advance(StdDuration::from_secs(RATE_RECOVERY_SECS)).await;
        let a = pool.acquire("claude-code").await;
        let b = pool.acquire("claude-code").await;
        let c = pool.acquire("claude-code").await;
        let pool3 = pool.clone();
        let fourth_task = tokio::spawn(async move { pool3.acquire("claude-code").await });
        tokio::task::yield_now().await;
        assert!(!fourth_task.is_finished(), "recovery must not exceed the base limit (3)");
        drop(a);
        drop(b);
        drop(c);
        let d = tokio::time::timeout(StdDuration::from_secs(1), fourth_task)
            .await
            .expect("fourth acquire should complete once a permit drops")
            .expect("task did not panic");
        drop(d);
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_lazy_evaluation_catches_up_multiple_elapsed_recovery_intervals() {
        let pool = pool_with("claude-code", 5);

        // Three reports, each spaced more than one cooldown apart, so each
        // independently reduces the limit: 5 -> 4 -> 3 -> 2.
        pool.report_rate_limit("claude-code");
        tokio::time::advance(StdDuration::from_secs(RATE_RECOVERY_SECS + 1)).await;
        pool.report_rate_limit("claude-code");
        tokio::time::advance(StdDuration::from_secs(RATE_RECOVERY_SECS + 1)).await;
        pool.report_rate_limit("claude-code");

        // No `acquire` calls happened while any of the above elapsed, so no
        // recovery has been applied yet: three whole `RATE_RECOVERY_SECS`
        // intervals now elapse in one stretch before the next `acquire`.
        tokio::time::advance(StdDuration::from_secs(3 * RATE_RECOVERY_SECS)).await;

        // A single lazy evaluation must catch up all 3 owed steps at once
        // (2 + 3 = 5 = base), not just +1. Sequentially acquiring 5 slots
        // (the full base) must not block; if recovery only ever added 1
        // step regardless of elapsed time, this would hang and the
        // surrounding timeout would fail the test instead.
        let permits: Vec<_> = tokio::time::timeout(StdDuration::from_secs(1), async {
            let mut acquired = Vec::new();
            for _ in 0..5 {
                acquired.push(pool.acquire("claude-code").await);
            }
            acquired
        })
        .await
        .expect(
            "after 3 elapsed recovery intervals, a single lazy evaluation should catch up all \
             owed steps at once and fully restore the limit to base (5)",
        );
        assert_eq!(permits.len(), 5);
        drop(permits);
    }
}
