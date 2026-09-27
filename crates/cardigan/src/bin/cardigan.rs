use std::{io, sync::Arc};

use anyhow::Context;
use cardigan::{
    carddav::build_address_book,
    commands::{CommandLine, Commands, Target},
    config::Config,
    dump,
    logging::init_logging,
    sync::{CycleRunner, NoopCycleRunner, run_daemon},
};
use cg_core::{
    ExternalServicesBuilder,
    contact::Side,
    create_services,
    repository::RepositoryService,
    service::{CycleMode, CycleRequest},
};
use cg_database::{create_repository_service, open_database};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli: CommandLine = clap::Parser::parse();
    match cli.command {
        Commands::Dump { target } => run_dump(target).await,
        command => run_engine(command).await,
    }
}

/// `dump` installs no logging, so stdout carries only the JSON, and never
/// opens the state database.
async fn run_dump(target: Target) -> anyhow::Result<()> {
    let config = Config::load().context("Cannot load configuration")?;
    let side = Side::from(target);
    let book = build_address_book(side, &config)?;
    let dump = dump::collect(side, &book).await?;
    dump::ignore_broken_pipe(dump::write_json(&dump, io::stdout().lock())).context("Couldn't write the dump")
}

/// `sync` and `dry-run`: logging, the state database and core services.
async fn run_engine(command: Commands) -> anyhow::Result<()> {
    init_logging()?;
    let config = Config::load().context("Cannot load configuration")?;

    tracing::info!("Cardigan {}", clap::crate_version!());

    let repository_service = open_repository(&config).await?;
    let external = ExternalServicesBuilder::default()
        .repository_service(repository_service.clone())
        .build()
        .context("ExternalServices missing required field")?;
    let _core_services = create_services(external).context("Couldn't create core services")?;

    // CG-9 replaces this with the core SyncService.
    let runner: Arc<dyn CycleRunner> = Arc::new(NoopCycleRunner);

    let result = match command {
        Commands::Sync { once: true, reset } => runner
            .run_cycle(CycleRequest { mode: CycleMode::Sync, reset })
            .await
            .map(drop)
            .map_err(Into::into),
        Commands::Sync { once: false, reset } => run_daemon(runner, config.poll_interval, reset).await,
        Commands::DryRun { reset } => runner
            .run_cycle(CycleRequest {
                mode: CycleMode::DryRun,
                reset,
            })
            .await
            .map(drop)
            .map_err(Into::into),
        Commands::Dump { .. } => unreachable!("dump is dispatched before the engine starts"),
    };

    match repository_service.repository().close().await.context("Couldn't close database") {
        Ok(()) => result,
        Err(close_err) => match result {
            Err(result_err) => {
                tracing::error!(error = %format_args!("{close_err:#}"), "Couldn't close database");
                Err(result_err)
            }
            Ok(()) => Err(close_err),
        },
    }
}

async fn open_repository(config: &Config) -> anyhow::Result<Arc<RepositoryService>> {
    let database_url = config.database_url()?;
    let database = open_database(&database_url).await.context("Couldn't create database connection")?;
    create_repository_service(database).await.context("Couldn't run database migrations")
}
