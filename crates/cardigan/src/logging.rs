use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

/// Dependency targets that are too noisy at Cardigan's default levels.
const SILENCED_TARGETS: &[&str] = &[
    "sqlx::query",
    "sea_orm",
    "warnings::warnings",
    "hyper::proto",
    "hyper::client",
    "hyper_util::client",
    "h2",
    "rustls",
    "tokio_util",
    "reqwest",
];

fn build_env_filter(base: EnvFilter) -> Result<EnvFilter> {
    SILENCED_TARGETS.iter().try_fold(base, |filter, target| {
        Ok(filter.add_directive(format!("{target}=off").parse().with_context(|| format!("Invalid log directive for {target}"))?))
    })
}

pub fn init_logging() -> Result<()> {
    use tracing::subscriber::set_global_default;
    use tracing_log::LogTracer;
    use tracing_subscriber::{Registry, fmt::format::FmtSpan, prelude::__tracing_subscriber_SubscriberExt};

    LogTracer::init_with_filter(log::LevelFilter::Off).context("Unable to setup log tracer")?;

    let env_filter = build_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))?;

    let formatting_layer = tracing_subscriber::fmt::layer()
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_ansi(false);

    let subscriber = Registry::default().with(env_filter).with(formatting_layer);

    set_global_default(subscriber).context("Failed to set tracing subscriber")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_filter_silences_only_cardigan_dependencies() {
        let filter = build_env_filter(EnvFilter::new("info")).unwrap().to_string();
        for target in [
            "hyper::proto",
            "hyper_util::client",
            "reqwest",
            "rustls",
            "h2",
            "tokio_util",
            "sea_orm",
            "sqlx::query",
        ] {
            assert!(filter.contains(&format!("{target}=off")), "{target} should be silenced in {filter}");
        }
        for leftover in ["axum", "tower_http", "simple_crypt", "sqlx::postgres"] {
            assert!(!filter.contains(leftover), "{leftover} is a BookBoss leftover in {filter}");
        }
    }
}
