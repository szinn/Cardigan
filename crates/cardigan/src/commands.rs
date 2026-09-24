#[derive(Debug, clap::Parser)]
#[command(
    name = "Cardigan",
    help_template = r#"
{before-help}{name} {version} - {about}

{usage-heading} {usage}

{all-args}{after-help}

AUTHORS:
    {author}
"#,
    version,
    author
)]
#[command(about, long_about = None)]
#[command(propagate_version = true, arg_required_else_help = true)]
pub struct CommandLine {
    #[clap(subcommand)]
    pub command: Commands,
}

#[derive(Debug, PartialEq, Eq, clap::Subcommand)]
pub enum Commands {
    #[command(about = "Sync iCloud and Fastmail contacts continuously", display_order = 10)]
    Sync {
        #[arg(long, help = "Run a single sync cycle and exit")]
        once: bool,
        #[arg(long, help = "Drop sync state and re-baseline before syncing")]
        reset: bool,
    },
    #[command(about = "Print the sync plan without changing either server or the sync state", display_order = 20)]
    DryRun {
        #[arg(long, help = "Preview a re-baseline from empty state")]
        reset: bool,
    },
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser, error::ErrorKind};

    use super::*;

    fn parse(args: &[&str]) -> Result<Commands, clap::Error> {
        CommandLine::try_parse_from(std::iter::once("cardigan").chain(args.iter().copied())).map(|cli| cli.command)
    }

    #[test]
    fn command_definition_is_valid() {
        CommandLine::command().debug_assert();
    }

    #[test]
    fn sync_defaults() {
        assert_eq!(parse(&["sync"]).unwrap(), Commands::Sync { once: false, reset: false });
    }

    #[test]
    fn sync_once_reset() {
        assert_eq!(parse(&["sync", "--once", "--reset"]).unwrap(), Commands::Sync { once: true, reset: true });
    }

    #[test]
    fn dry_run_defaults() {
        assert_eq!(parse(&["dry-run"]).unwrap(), Commands::DryRun { reset: false });
    }

    #[test]
    fn dry_run_reset() {
        assert_eq!(parse(&["dry-run", "--reset"]).unwrap(), Commands::DryRun { reset: true });
    }

    #[test]
    fn rejects_missing_subcommand() {
        assert_eq!(parse(&[]).unwrap_err().kind(), ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand);
    }

    #[test]
    fn rejects_old_server_subcommand() {
        assert_eq!(parse(&["server"]).unwrap_err().kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn rejects_once_on_dry_run() {
        assert_eq!(parse(&["dry-run", "--once"]).unwrap_err().kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn rejects_positional_arguments() {
        assert_eq!(parse(&["sync", "icloud"]).unwrap_err().kind(), ErrorKind::UnknownArgument);
    }
}
