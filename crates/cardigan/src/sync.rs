//! The sync daemon loop.
//!
//! The loop only schedules cycles; what a cycle does is behind [`CycleRunner`],
//! which the core `SyncService` implements below.

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use cg_core::{
    AddressBookError, Error,
    service::{CycleMode, CycleOutcome, CycleRequest, SyncService},
};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemBuilder, SubsystemHandle, Toplevel};
use tokio_util::sync::CancellationToken;

/// How long `Toplevel` waits for the in-flight cycle after a shutdown signal.
/// Stays under Docker's default 10s stop grace period (and launchd's default
/// 20s ExitTimeOut), so the timed-out path can still close the database
/// before SIGKILL; a cycle cut off here is safe, because the next cycle
/// adopts whatever it already wrote.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(8);

/// One sync cycle.
#[async_trait::async_trait]
pub trait CycleRunner: Send + Sync {
    async fn run_cycle(&self, request: CycleRequest) -> Result<CycleOutcome, Error>;
}

#[async_trait::async_trait]
impl CycleRunner for SyncService {
    async fn run_cycle(&self, request: CycleRequest) -> Result<CycleOutcome, Error> {
        Self::run_cycle(self, request).await
    }
}

/// The server's `Retry-After`, when a cycle failed because it was rate limited.
fn retry_after(error: &Error) -> Option<Duration> {
    match error {
        Error::AddressBook(AddressBookError::RateLimited { retry_after }) => *retry_after,
        _ => None,
    }
}

/// Runs a cycle immediately and then every `poll_interval` until `shutdown`
/// is cancelled. An in-flight cycle always completes before the loop exits.
/// Failed cycles are logged and retried on the next tick, or after the
/// server's `Retry-After` when that is later. `reset` stays set until a cycle
/// is idle or applied; a cycle the mass-deletion guard blocked keeps it.
pub async fn run_loop(runner: Arc<dyn CycleRunner>, poll_interval: Duration, reset: bool, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut reset = reset;

    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            _ = interval.tick() => {}
        }

        let started = Instant::now();
        match runner.run_cycle(CycleRequest { mode: CycleMode::Sync, reset }).await {
            // The SyncService has already logged why; try again next tick.
            // A `--reset` cycle clears state before planning, so in practice
            // only non-reset cycles are blocked; keeping `reset` here is the
            // loop's general policy.
            Ok(CycleOutcome::Blocked(_)) => {}
            Ok(_) => reset = false,
            Err(e) => {
                if let Some(delay) = retry_after(&e) {
                    let next = (started + poll_interval).max(Instant::now() + delay);
                    interval.reset_at(next);
                    tracing::error!(
                        error = %format_args!("{e:#}"),
                        retry_after_secs = delay.as_secs(),
                        "Sync cycle rate limited; delaying the next cycle"
                    );
                } else {
                    tracing::error!(error = %format_args!("{e:#}"), "Sync cycle failed; retrying next interval");
                }
            }
        }
    }

    tracing::info!("Sync loop stopped");
}

pub struct SyncSubsystem {
    runner: Arc<dyn CycleRunner>,
    poll_interval: Duration,
    reset: bool,
}

impl SyncSubsystem {
    pub fn new(runner: Arc<dyn CycleRunner>, poll_interval: Duration, reset: bool) -> Self {
        Self { runner, poll_interval, reset }
    }
}

impl IntoSubsystem<anyhow::Error> for SyncSubsystem {
    async fn run(self, subsys: &mut SubsystemHandle) -> anyhow::Result<()> {
        run_loop(self.runner, self.poll_interval, self.reset, subsys.create_cancellation_token()).await;
        Ok(())
    }
}

/// Runs the sync loop as a daemon until SIGINT/SIGTERM.
pub async fn run_daemon(runner: Arc<dyn CycleRunner>, poll_interval: Duration, reset: bool) -> anyhow::Result<()> {
    let subsystem = SyncSubsystem::new(runner, poll_interval, reset);

    Toplevel::new(async move |s: &mut SubsystemHandle| {
        s.start(SubsystemBuilder::new("Sync", subsystem.into_subsystem()));
    })
    .catch_signals()
    .handle_shutdown_requests(SHUTDOWN_TIMEOUT)
    .await
    .context("Sync daemon did not shut down cleanly")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    // `Instant`, `Error`, `AddressBookError` and `CycleOutcome` come from
    // `super::*`.
    use cg_core::{contact::Side, sync::MassDeletion};

    use super::*;

    /// Records every request with its start time; optionally sleeps per cycle.
    /// Returns the scripted results in order, then `Idle`.
    #[derive(Default)]
    struct RecordingRunner {
        requests: Mutex<Vec<(Instant, CycleRequest)>>,
        completed: AtomicUsize,
        script: Mutex<VecDeque<Result<CycleOutcome, Error>>>,
        cycle_duration: Duration,
    }

    impl RecordingRunner {
        fn scripted(results: impl IntoIterator<Item = Result<CycleOutcome, Error>>) -> Self {
            Self {
                script: Mutex::new(results.into_iter().collect()),
                ..Default::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl CycleRunner for RecordingRunner {
        async fn run_cycle(&self, request: CycleRequest) -> Result<CycleOutcome, Error> {
            self.requests.lock().unwrap().push((Instant::now(), request));
            if !self.cycle_duration.is_zero() {
                tokio::time::sleep(self.cycle_duration).await;
            }
            self.completed.fetch_add(1, Ordering::SeqCst);
            self.script.lock().unwrap().pop_front().unwrap_or(Ok(CycleOutcome::Idle))
        }
    }

    fn rate_limited(retry_after: Option<Duration>) -> Error {
        Error::AddressBook(AddressBookError::RateLimited { retry_after })
    }

    fn starts(cycles: &[(u64, CycleRequest)]) -> Vec<u64> {
        cycles.iter().map(|(t, _)| *t).collect()
    }

    /// Runs the loop for `run_time` of (paused) time, then cancels and waits
    /// for it to stop. Returns (seconds since start, request) per cycle.
    async fn run_for(runner: &Arc<RecordingRunner>, poll_interval: Duration, reset: bool, run_time: Duration) -> Vec<(u64, CycleRequest)> {
        let start = Instant::now();
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_loop(runner.clone(), poll_interval, reset, token.clone()));
        tokio::time::sleep(run_time).await;
        token.cancel();
        handle.await.unwrap();
        runner.requests.lock().unwrap().iter().map(|(t, r)| ((*t - start).as_secs(), *r)).collect()
    }

    const POLL: Duration = Duration::from_secs(60);

    fn sync(reset: bool) -> CycleRequest {
        CycleRequest { mode: CycleMode::Sync, reset }
    }

    #[tokio::test(start_paused = true)]
    async fn ticks_every_poll_interval_starting_immediately() {
        let runner = Arc::new(RecordingRunner::default());
        let cycles = run_for(&runner, POLL, false, Duration::from_secs(121)).await;
        assert_eq!(cycles, vec![(0, sync(false)), (60, sync(false)), (120, sync(false))]);
    }

    #[tokio::test(start_paused = true)]
    async fn reset_applies_to_first_successful_cycle_only() {
        let runner = Arc::new(RecordingRunner::default());
        let cycles = run_for(&runner, POLL, true, Duration::from_secs(121)).await;
        assert_eq!(cycles, vec![(0, sync(true)), (60, sync(false)), (120, sync(false))]);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_cycle_keeps_loop_running_and_retains_reset() {
        let runner = Arc::new(RecordingRunner::scripted([Err(Error::Infrastructure("state store down".into()))]));
        let cycles = run_for(&runner, POLL, true, Duration::from_secs(121)).await;
        assert_eq!(cycles, vec![(0, sync(true)), (60, sync(true)), (120, sync(false))]);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_waits_for_in_flight_cycle() {
        let runner = Arc::new(RecordingRunner {
            cycle_duration: Duration::from_secs(30),
            ..Default::default()
        });
        let start = Instant::now();
        let cycles = run_for(&runner, POLL, false, Duration::from_secs(10)).await;
        assert_eq!(cycles, vec![(0, sync(false))]);
        assert_eq!(runner.completed.load(Ordering::SeqCst), 1, "in-flight cycle must finish");
        assert_eq!(start.elapsed().as_secs(), 30, "loop exits as soon as the cycle finishes");
    }

    #[tokio::test(start_paused = true)]
    async fn overrunning_cycle_does_not_burst() {
        let runner = Arc::new(RecordingRunner {
            cycle_duration: Duration::from_secs(150),
            ..Default::default()
        });
        let cycles = run_for(&runner, POLL, false, Duration::from_secs(400)).await;
        assert_eq!(starts(&cycles), vec![0, 150, 300]);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_cycle_waits_for_retry_after() {
        let runner = Arc::new(RecordingRunner::scripted([Err(rate_limited(Some(Duration::from_secs(150))))]));
        let cycles = run_for(&runner, POLL, false, Duration::from_secs(211)).await;
        assert_eq!(starts(&cycles), vec![0, 150, 210], "next cycle at retry_after, then every poll interval");
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_shorter_than_poll_interval_keeps_the_interval() {
        let runner = Arc::new(RecordingRunner::scripted([Err(rate_limited(Some(Duration::from_secs(30))))]));
        let cycles = run_for(&runner, POLL, false, Duration::from_secs(121)).await;
        assert_eq!(starts(&cycles), vec![0, 60, 120]);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_without_retry_after_keeps_the_interval() {
        let runner = Arc::new(RecordingRunner::scripted([Err(rate_limited(None))]));
        let cycles = run_for(&runner, POLL, false, Duration::from_secs(121)).await;
        assert_eq!(starts(&cycles), vec![0, 60, 120]);
    }

    #[tokio::test(start_paused = true)]
    async fn long_retry_after_still_stops_on_shutdown() {
        let runner = Arc::new(RecordingRunner::scripted([Err(rate_limited(Some(Duration::from_hours(24))))]));
        let start = Instant::now();
        let cycles = run_for(&runner, POLL, false, Duration::from_secs(100)).await;
        assert_eq!(starts(&cycles), vec![0]);
        assert_eq!(start.elapsed().as_secs(), 100, "shutdown must not wait out retry_after");
    }

    #[tokio::test(start_paused = true)]
    async fn unauthorized_cycle_keeps_loop_running() {
        let runner = Arc::new(RecordingRunner::scripted([
            Err(Error::AddressBook(AddressBookError::Unauthorized)),
            Err(Error::AddressBook(AddressBookError::Unauthorized)),
        ]));
        let cycles = run_for(&runner, POLL, false, Duration::from_secs(121)).await;
        assert_eq!(starts(&cycles), vec![0, 60, 120], "a rotated password must not need a restart");
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_cycle_keeps_reset() {
        let blocked = MassDeletion {
            side: Side::Fastmail,
            deletes: 50,
            contacts: 100,
            limit: 20,
        };
        let runner = Arc::new(RecordingRunner::scripted([Ok(CycleOutcome::Blocked(blocked))]));
        let cycles = run_for(&runner, POLL, true, Duration::from_secs(121)).await;
        assert_eq!(cycles, vec![(0, sync(true)), (60, sync(true)), (120, sync(false))]);
    }

    #[tokio::test(start_paused = true)]
    async fn sync_subsystem_runs_under_toplevel_until_shutdown_requested() {
        let runner = Arc::new(RecordingRunner::default());
        let subsystem = SyncSubsystem::new(runner.clone(), POLL, false);

        Toplevel::new(async move |s: &mut SubsystemHandle| {
            s.start(SubsystemBuilder::new("Sync", subsystem.into_subsystem()));
            s.start(SubsystemBuilder::new(
                "ShutdownTrigger",
                async move |sub: &mut SubsystemHandle| -> anyhow::Result<()> {
                    // Let the sync loop run its first (immediate) cycle and one
                    // more after a full poll interval, then stop the tree.
                    tokio::time::sleep(POLL + Duration::from_secs(1)).await;
                    sub.request_shutdown();
                    Ok(())
                },
            ));
        })
        .handle_shutdown_requests(SHUTDOWN_TIMEOUT)
        .await
        .expect("toplevel should shut down cleanly");

        let cycles: Vec<CycleRequest> = runner.requests.lock().unwrap().iter().map(|(_, r)| *r).collect();
        assert_eq!(cycles, vec![sync(false), sync(false)], "expected the immediate cycle plus one more tick");
    }
}
