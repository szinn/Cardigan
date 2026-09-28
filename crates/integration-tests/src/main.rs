//! Integration tests against a real CardDAV server: Radicale in Docker, one
//! container per test. Every test is `#[ignore]`d; run them with
//! `mise run integration-tests` (needs a running docker/colima daemon).

mod clock;
mod faulty;
mod harness;
mod propagation;
mod radicale;
