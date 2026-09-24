use std::sync::Arc;

use anyhow::Context;
use cardigan::{
    commands::{CommandLine, Commands},
    config::Config,
    logging::init_logging,
    sync::{CycleMode, CycleRequest, CycleRunner, NoopCycleRunner, run_daemon},
};
use cg_core::{ExternalServicesBuilder, create_services, repository::RepositoryService};
use cg_database::{create_repository_service, open_database};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli: CommandLine = clap::Parser::parse();
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

    let result = match cli.command {
        Commands::Sync { once: true, reset } => runner.run_cycle(CycleRequest { mode: CycleMode::Sync, reset }).await,
        Commands::Sync { once: false, reset } => run_daemon(runner, config.poll_interval, reset).await,
        Commands::DryRun { reset } => {
            runner
                .run_cycle(CycleRequest {
                    mode: CycleMode::DryRun,
                    reset,
                })
                .await
        }
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
