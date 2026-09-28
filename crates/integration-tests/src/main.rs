//! Integration tests against a real CardDAV server: Radicale in Docker, one
//! container per test. Every test is `#[ignore]`d; run them with
//! `mise run integration-tests` (needs a running docker/colima daemon).

mod baseline;
mod clock;
mod conflict;
mod faulty;
mod harness;
mod idle;
mod propagation;
mod radicale;
mod token;
