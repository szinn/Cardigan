//! The sync daemon loop.
//!
//! The loop only schedules cycles; what a cycle does is behind [`CycleRunner`],
//! which CG-9 implements with the core `SyncService`. Until then the binary
//! uses [`NoopCycleRunner`].

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use tokio::time::MissedTickBehavior;
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemBuilder, SubsystemHandle, Toplevel};
use tokio_util::sync::CancellationToken;

/// How long `Toplevel` waits for the in-flight cycle after a shutdown signal.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether a cycle may write to the servers and the state tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleMode {
    /// Apply the plan.
    Sync,
    /// Compute and print the plan; change no server or table data.
    DryRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleRequest {
    pub mode: CycleMode,
    /// Drop state and re-baseline (in `DryRun`, only preview the re-baseline).
    pub reset: bool,
}

/// One sync cycle. Implemented by the core `SyncService` in CG-9.
#[async_trait::async_trait]
pub trait CycleRunner: Send + Sync {
    async fn run_cycle(&self, request: CycleRequest) -> anyhow::Result<()>;
}

/// Placeholder runner until CG-9 wires in the `SyncService`.
pub struct NoopCycleRunner;

#[async_trait::async_trait]
impl CycleRunner for NoopCycleRunner {
    async fn run_cycle(&self, request: CycleRequest) -> anyhow::Result<()> {
        tracing::debug!(mode = ?request.mode, reset = request.reset, "Sync cycle (no-op)");
        Ok(())
    }
}

/// Runs a cycle immediately and then every `poll_interval` until `shutdown`
/// is cancelled. An in-flight cycle always completes before the loop exits.
/// Failed cycles are logged and retried on the next tick; `reset` stays set
/// until a cycle succeeds.
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

        match runner.run_cycle(CycleRequest { mode: CycleMode::Sync, reset }).await {
            Ok(()) => reset = false,
            Err(e) => tracing::error!(error = %e, "Sync cycle failed; retrying next interval"),
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
    // `Arc`, `Duration` and `CancellationToken` come from `super::*`; importing
    // them again trips the workspace `redundant_imports` lint.
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::time::Instant;

    use super::*;

    /// Records every request with its start time; optionally sleeps per cycle
    /// and fails the first `fail_first` calls.
    #[derive(Default)]
    struct RecordingRunner {
        requests: Mutex<Vec<(Instant, CycleRequest)>>,
        completed: AtomicUsize,
        fail_first: usize,
        cycle_duration: Duration,
    }

    #[async_trait::async_trait]
    impl CycleRunner for RecordingRunner {
        async fn run_cycle(&self, request: CycleRequest) -> anyhow::Result<()> {
            let call = {
                let mut requests = self.requests.lock().unwrap();
                requests.push((Instant::now(), request));
                requests.len()
            };
            if !self.cycle_duration.is_zero() {
                tokio::time::sleep(self.cycle_duration).await;
            }
            self.completed.fetch_add(1, Ordering::SeqCst);
            if call <= self.fail_first {
                anyhow::bail!("simulated failure {call}");
            }
            Ok(())
        }
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
        let runner = Arc::new(RecordingRunner {
            fail_first: 1,
            ..Default::default()
        });
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
        let starts: Vec<u64> = cycles.iter().map(|(t, _)| *t).collect();
        assert_eq!(starts, vec![0, 150, 300]);
    }

    #[tokio::test]
    async fn noop_runner_succeeds() {
        NoopCycleRunner
            .run_cycle(CycleRequest {
                mode: CycleMode::DryRun,
                reset: true,
            })
            .await
            .unwrap();
    }
}
