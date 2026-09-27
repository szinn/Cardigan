//! What `sync --once` and `dry-run` report when their single cycle ends.

use std::{fmt, io};

use anyhow::Context;
use cg_core::{
    service::{CycleMode, CycleOutcome, CycleRequest, CycleSummary, DirectionCounts},
    sync::{CyclePlan, MassDeletion},
};

use crate::{dump::ignore_broken_pipe, sync::CycleRunner};

/// Runs the one cycle of `sync --once` or `dry-run`. A dry run writes its
/// report to `out` in a single write, so log lines cannot split it, and
/// succeeds even when the mass-deletion guard would block the sync.
/// `sync --once` prints nothing (the core logs the cycle summary) and fails
/// when the guard blocked the cycle, so cron sees a non-zero exit.
pub async fn run_once(runner: &dyn CycleRunner, request: CycleRequest, out: &mut dyn io::Write) -> anyhow::Result<()> {
    let outcome = runner.run_cycle(request).await.context("Sync cycle failed")?;
    match (request.mode, outcome) {
        (CycleMode::DryRun, CycleOutcome::DryRun { cycle, blocked }) => {
            let report = DryRunReport {
                cycle: &cycle,
                blocked: blocked.as_ref(),
            }
            .to_string();
            ignore_broken_pipe(out.write_all(report.as_bytes()).and_then(|()| out.flush())).context("Couldn't write the dry-run plan")
        }
        (CycleMode::Sync, CycleOutcome::Idle | CycleOutcome::Applied(_)) => Ok(()),
        (CycleMode::Sync, CycleOutcome::Blocked(blocked)) => {
            anyhow::bail!("{blocked}; nothing was written. Run `cardigan dry-run` to see the plan")
        }
        // Only the variant name: a CyclePlan's Debug carries card content.
        (mode, outcome) => anyhow::bail!("{mode:?} cycle returned an unexpected {} outcome", variant(&outcome)),
    }
}

fn variant(outcome: &CycleOutcome) -> &'static str {
    match outcome {
        CycleOutcome::Idle => "Idle",
        CycleOutcome::DryRun { .. } => "DryRun",
        CycleOutcome::Blocked(_) => "Blocked",
        CycleOutcome::Applied(_) => "Applied",
    }
}

/// A dry run's report: the guard's verdict, the counts per direction, every
/// op and diagnostic, then the baseline report. Uses only the core types'
/// PII-safe `Display` impls.
pub struct DryRunReport<'a> {
    pub cycle: &'a CyclePlan,
    pub blocked: Option<&'a MassDeletion>,
}

impl fmt::Display for DryRunReport<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Dry run: nothing was written.")?;
        if let Some(blocked) = self.blocked {
            writeln!(f, "BLOCKED: {blocked}; `cardigan sync` would write nothing.")?;
        }
        let counts = CycleSummary::planned(&self.cycle.plan.ops);
        writeln!(f, "icloud→fastmail: {}", Planned(&counts.to_fastmail))?;
        writeln!(f, "fastmail→icloud: {}", Planned(&counts.to_icloud))?;
        writeln!(
            f,
            "state only: adopt {}, refresh {}, forget {}",
            counts.adopted, counts.refreshed, counts.forgotten
        )?;
        writeln!(f, "\nPlan:")?;
        write!(f, "{}", self.cycle.plan)?;
        writeln!(f, "\nBaseline report:")?;
        write!(f, "{}", self.cycle.report)
    }
}

/// One direction's planned writes.
struct Planned<'a>(&'a DirectionCounts);

impl fmt::Display for Planned<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = self.0;
        write!(f, "create {}, update {}, delete {}, conflicts {}", c.added, c.updated, c.removed, c.conflicts)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use cg_core::{AddressBookError, Error, contact::Side};

    use super::*;

    /// Returns one scripted result and records the request it was given.
    struct Stub {
        result: Mutex<Option<Result<CycleOutcome, Error>>>,
        requests: Mutex<Vec<CycleRequest>>,
    }

    impl Stub {
        fn new(result: Result<CycleOutcome, Error>) -> Self {
            Self {
                result: Mutex::new(Some(result)),
                requests: Mutex::default(),
            }
        }
    }

    #[async_trait::async_trait]
    impl CycleRunner for Stub {
        async fn run_cycle(&self, request: CycleRequest) -> Result<CycleOutcome, Error> {
            self.requests.lock().unwrap().push(request);
            self.result.lock().unwrap().take().expect("one cycle per test")
        }
    }

    const SYNC: CycleRequest = CycleRequest {
        mode: CycleMode::Sync,
        reset: false,
    };
    const DRY_RUN: CycleRequest = CycleRequest {
        mode: CycleMode::DryRun,
        reset: false,
    };

    fn blocked() -> MassDeletion {
        MassDeletion {
            side: Side::Fastmail,
            deletes: 50,
            contacts: 100,
            limit: 20,
        }
    }

    async fn run(runner: &Stub, request: CycleRequest) -> (anyhow::Result<()>, String) {
        let mut out = Vec::new();
        let result = run_once(runner, request, &mut out).await;
        (result, String::from_utf8(out).unwrap())
    }

    #[tokio::test]
    async fn sync_once_succeeds_when_applied_or_idle() {
        for outcome in [CycleOutcome::Idle, CycleOutcome::Applied(CycleSummary::default())] {
            let (result, out) = run(&Stub::new(Ok(outcome)), SYNC).await;
            result.unwrap();
            assert!(out.is_empty(), "sync --once prints nothing to stdout; the core logs the summary");
        }
    }

    #[tokio::test]
    async fn sync_once_fails_when_blocked() {
        let (result, out) = run(&Stub::new(Ok(CycleOutcome::Blocked(blocked()))), SYNC).await;
        let message = format!("{:#}", result.unwrap_err());
        assert!(
            message.contains("plan would delete 50 of 100 synced contacts on fastmail (limit 20)"),
            "{message}"
        );
        assert!(message.contains("cardigan dry-run"), "{message}");
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn cycle_error_is_returned() {
        let (result, _) = run(&Stub::new(Err(Error::AddressBook(AddressBookError::Unauthorized))), SYNC).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("CardDAV authentication failed"));
    }

    #[tokio::test]
    async fn dry_run_refuses_a_non_dry_run_outcome_without_writing() {
        let (result, out) = run(&Stub::new(Ok(CycleOutcome::Applied(CycleSummary::default()))), DRY_RUN).await;
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("Applied"), "{message}");
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn dry_run_passes_the_request_through() {
        let stub = Stub::new(Ok(CycleOutcome::DryRun {
            cycle: CyclePlan::default(),
            blocked: None,
        }));
        let request = CycleRequest {
            mode: CycleMode::DryRun,
            reset: true,
        };
        let (result, _) = run(&stub, request).await;
        result.unwrap();
        assert_eq!(*stub.requests.lock().unwrap(), vec![request]);
    }

    #[tokio::test]
    async fn blocked_dry_run_succeeds_and_says_so_first() {
        let stub = Stub::new(Ok(CycleOutcome::DryRun {
            cycle: CyclePlan::default(),
            blocked: Some(blocked()),
        }));
        let (result, out) = run(&stub, DRY_RUN).await;
        result.unwrap();
        insta::assert_snapshot!(out);
    }

    struct BrokenPipe;

    impl io::Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    #[tokio::test]
    async fn run_once_ignores_broken_pipe() {
        let stub = Stub::new(Ok(CycleOutcome::DryRun {
            cycle: CyclePlan::default(),
            blocked: None,
        }));
        run_once(&stub, DRY_RUN, &mut BrokenPipe).await.unwrap();
    }
}
