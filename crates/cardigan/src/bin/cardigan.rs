use cardigan::{
    commands::{CommandLine, Commands},
    config::Config,
    logging::init_logging,
};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use anyhow::Context;

    let cli: CommandLine = clap::Parser::parse();
    let config = Config::load().context("Cannot load configuration")?;

    match cli.command {
        Commands::Server => {
            init_logging()?;
            cmd_server(config).await
        }
    }
}

async fn cmd_server(_config: cardigan::config::Config) -> anyhow::Result<()> {
    // use std::{sync::Arc, time::Duration};
    //
    // use anyhow::Context;
    // use bb_api::create_api_subsystem;
    // use bb_core::{ExternalServicesBuilder, create_core_subsystem,
    // create_services, format::FormatService};
    // use bb_database::{create_repository_service, open_database};
    // use bb_formats::create_format_service;
    // use bb_frontend::server::create_frontend_subsystem;
    // use bb_metadata::before_start as metadata_before_start;
    // use tokio_graceful_shutdown::{IntoSubsystem, SubsystemBuilder,
    // SubsystemHandle, Toplevel};
    //
    tracing::info!("Cardigan {}", clap::crate_version!());

    let span = tracing::span!(tracing::Level::TRACE, "Cardigan Startup").entered();

    // let database = open_database(&config.database).await.context("Couldn't
    // create database connection")?; let repository_service =
    // create_repository_service(database).await.context("Couldn't create
    // database connection")?; let file_store =
    // Arc::new(bb_storage::LocalFileStore::new(config.library.library_path.
    // clone())); let format_service: Arc<dyn FormatService> =
    // Arc::new(create_format_service()); let worker_poll_interval =
    // Duration::from_secs(config.import.worker_poll_interval_secs);
    //
    // let external = ExternalServicesBuilder::default()
    //     .repository_service(repository_service.clone())
    //     .file_store(file_store)
    //     .format_service(format_service)
    //     .bookdrop_path(config.import.bookdrop_path.clone())
    //     .scan_interval(Duration::from_secs(config.import.scan_interval_secs))
    //     .build()
    //     .context("ExternalServices missing required field")?;
    // let core_services = create_services(external,
    // &config.encryption_secret).context("Couldn't create core services")?;

    // Each crate self-registers its job handlers and health task configs.
    // bb_core::before_start(&core_services);
    // Register configured metadata providers into the metadata service.
    // metadata_before_start(&core_services, &config.metadata);

    // let api_subsystem = create_api_subsystem(&config.api,
    // core_services.clone()); let core_subsystem =
    // create_core_subsystem(core_services.clone(), worker_poll_interval);
    // let core_subsystem = bb_core::ResilienceWrapper::new("Core",
    // core_subsystem, core_services.system_message_service.clone());
    // let oidc_config = if config.oidc.is_set() { Some(config.oidc.clone()) }
    // else { None }; let frontend_subsystem =
    // create_frontend_subsystem(&config.frontend, oidc_config,
    // core_services.clone());

    span.exit();

    // Toplevel::new(async |s: &mut SubsystemHandle| {
    //     s.start(SubsystemBuilder::new("Api",
    // api_subsystem.into_subsystem()));     s.start(SubsystemBuilder::new("
    // Core", core_subsystem.into_subsystem()));
    //     s.start(SubsystemBuilder::new("Frontend",
    // frontend_subsystem.into_subsystem())); })
    // .catch_signals()
    // .handle_shutdown_requests(Duration::from_secs(3))
    // .await?;

    // repository_service.repository().close().await.context("Couldn't close
    // database")?;
    Ok(())
}
