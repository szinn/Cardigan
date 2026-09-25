use std::{fmt, path::PathBuf, str::FromStr, time::Duration};

use cg_core::contact::ConflictWinner;
use serde::Deserialize;

use crate::error::Error;

const ENV_PREFIX: &str = "CARDIGAN";

pub const DEFAULT_ICLOUD_URL: &str = "https://contacts.icloud.com";
pub const DEFAULT_FASTMAIL_URL: &str = "https://carddav.fastmail.com";
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;
pub const MIN_POLL_INTERVAL_SECS: u64 = 60;
pub const DEFAULT_MAX_PHOTO_BYTES: u64 = 1024 * 1024;

/// Validated runtime configuration, loaded entirely from `CARDIGAN_*`
/// environment variables.
#[derive(Debug)]
pub struct Config {
    pub icloud: EndpointConfig,
    pub fastmail: EndpointConfig,
    pub poll_interval: Duration,
    pub database_path: PathBuf,
    pub conflict_winner: ConflictWinner,
    pub max_photo_bytes: u64,
}

/// Connection settings for one CardDAV endpoint. The URL is the discovery
/// entry point; principal discovery still resolves the real host.
#[derive(Debug)]
pub struct EndpointConfig {
    pub url: String,
    pub username: String,
    pub password: Secret,
}

/// A credential that must never appear in logs. `Debug` is redacted; use
/// [`Secret::expose`] only at the point the value is sent to a server.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// Raw environment values, before defaults and validation. Every field is a
/// string so that parse failures can name the offending variable.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    icloud_url: Option<String>,
    icloud_username: Option<String>,
    icloud_password: Option<String>,
    fastmail_url: Option<String>,
    fastmail_username: Option<String>,
    fastmail_password: Option<String>,
    poll_interval_secs: Option<String>,
    database_path: Option<String>,
    conflict_winner: Option<String>,
    max_photo_bytes: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self, Error> {
        Self::load_from(config::Environment::with_prefix(ENV_PREFIX))
    }

    fn load_from(environment: config::Environment) -> Result<Self, Error> {
        let raw: RawConfig = config::Config::builder().add_source(environment).build()?.try_deserialize()?;

        Self::try_from(raw)
    }

    /// SQLite URL for the state database inside `database_path`. The
    /// directory must already exist; the file is created on first open.
    pub fn database_url(&self) -> Result<String, Error> {
        let path = self.database_path.display().to_string();
        if path.contains('?') || path.contains('%') {
            return Err(Error::InvalidValue {
                variable: "CARDIGAN_DATABASE_PATH",
                reason: format!("{path} contains '?' or '%', which the SQLite URL cannot represent"),
            });
        }

        if !self.database_path.is_dir() {
            return Err(Error::InvalidValue {
                variable: "CARDIGAN_DATABASE_PATH",
                reason: format!("{} is not an existing directory", self.database_path.display()),
            });
        }

        Ok(format!("sqlite://{}?mode=rwc", self.database_path.join("cardigan.db").display()))
    }
}

impl TryFrom<RawConfig> for Config {
    type Error = Error;

    fn try_from(raw: RawConfig) -> Result<Self, Self::Error> {
        let mut missing = Vec::new();
        let mut required = |value: Option<String>, variable: &'static str| {
            let value = non_empty(value);
            if value.is_none() {
                missing.push(variable);
            }
            value.unwrap_or_default()
        };

        let icloud_username = required(raw.icloud_username, "CARDIGAN_ICLOUD_USERNAME");
        let icloud_password = required(raw.icloud_password, "CARDIGAN_ICLOUD_PASSWORD");
        let fastmail_username = required(raw.fastmail_username, "CARDIGAN_FASTMAIL_USERNAME");
        let fastmail_password = required(raw.fastmail_password, "CARDIGAN_FASTMAIL_PASSWORD");
        let database_path = required(raw.database_path, "CARDIGAN_DATABASE_PATH");

        if !missing.is_empty() {
            return Err(Error::MissingVariables(missing));
        }

        let poll_interval_secs = parse_or(raw.poll_interval_secs, "CARDIGAN_POLL_INTERVAL_SECS", DEFAULT_POLL_INTERVAL_SECS)?;
        if poll_interval_secs < MIN_POLL_INTERVAL_SECS {
            return Err(Error::InvalidValue {
                variable: "CARDIGAN_POLL_INTERVAL_SECS",
                reason: format!("must be at least {MIN_POLL_INTERVAL_SECS} seconds, got {poll_interval_secs}"),
            });
        }

        Ok(Self {
            icloud: EndpointConfig {
                url: non_empty(raw.icloud_url).unwrap_or_else(|| DEFAULT_ICLOUD_URL.to_string()),
                username: icloud_username,
                password: Secret::new(icloud_password),
            },
            fastmail: EndpointConfig {
                url: non_empty(raw.fastmail_url).unwrap_or_else(|| DEFAULT_FASTMAIL_URL.to_string()),
                username: fastmail_username,
                password: Secret::new(fastmail_password),
            },
            poll_interval: Duration::from_secs(poll_interval_secs),
            database_path: PathBuf::from(database_path),
            conflict_winner: parse_or(raw.conflict_winner, "CARDIGAN_CONFLICT_WINNER", ConflictWinner::ICloud)?,
            max_photo_bytes: parse_or(raw.max_photo_bytes, "CARDIGAN_MAX_PHOTO_BYTES", DEFAULT_MAX_PHOTO_BYTES)?,
        })
    }
}

/// Treats unset and blank variables alike.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Parses an optional variable, falling back to `default` when unset.
/// Only used for non-secret variables, so the error may echo the value.
fn parse_or<T>(value: Option<String>, variable: &'static str, default: T) -> Result<T, Error>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    match non_empty(value) {
        None => Ok(default),
        Some(value) => value.trim().parse().map_err(|e: T::Err| Error::InvalidValue {
            variable,
            reason: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(vars: &[(&str, &str)]) -> Result<Config, Error> {
        let source = vars.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        Config::load_from(config::Environment::with_prefix(ENV_PREFIX).source(Some(source)))
    }

    const REQUIRED: &[(&str, &str)] = &[
        ("CARDIGAN_ICLOUD_USERNAME", "jane@icloud.com"),
        ("CARDIGAN_ICLOUD_PASSWORD", "icloud-secret-pw"),
        ("CARDIGAN_FASTMAIL_USERNAME", "jane@fastmail.com"),
        ("CARDIGAN_FASTMAIL_PASSWORD", "fastmail-secret-pw"),
        ("CARDIGAN_DATABASE_PATH", "/var/lib/cardigan"),
    ];

    fn with(overrides: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        let mut vars: Vec<_> = REQUIRED.iter().filter(|(k, _)| !overrides.iter().any(|(o, _)| o == k)).copied().collect();
        vars.extend_from_slice(overrides);
        vars
    }

    #[test]
    fn required_only_applies_defaults() {
        let config = load(REQUIRED).unwrap();
        insta::assert_debug_snapshot!(config);
    }

    #[test]
    fn all_variables_set() {
        let config = load(&with(&[
            ("CARDIGAN_ICLOUD_URL", "http://localhost:5232/icloud"),
            ("CARDIGAN_FASTMAIL_URL", "http://localhost:5232/fastmail"),
            ("CARDIGAN_POLL_INTERVAL_SECS", "300"),
            ("CARDIGAN_CONFLICT_WINNER", "Fastmail"),
            ("CARDIGAN_MAX_PHOTO_BYTES", "2048"),
        ]))
        .unwrap();
        insta::assert_debug_snapshot!(config);
    }

    #[test]
    fn debug_never_shows_passwords() {
        let debug = format!("{:?}", load(REQUIRED).unwrap());
        assert!(!debug.contains("icloud-secret-pw"));
        assert!(!debug.contains("fastmail-secret-pw"));
    }

    #[test]
    fn missing_required_variables_are_all_named() {
        let err = load(&[]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "missing required environment variable(s): CARDIGAN_ICLOUD_USERNAME, CARDIGAN_ICLOUD_PASSWORD, CARDIGAN_FASTMAIL_USERNAME, \
             CARDIGAN_FASTMAIL_PASSWORD, CARDIGAN_DATABASE_PATH"
        );
    }

    #[test]
    fn blank_required_variable_is_missing() {
        let err = load(&with(&[("CARDIGAN_ICLOUD_PASSWORD", "  ")])).unwrap_err();
        assert_eq!(err.to_string(), "missing required environment variable(s): CARDIGAN_ICLOUD_PASSWORD");
    }

    #[test]
    fn double_underscore_prefix_is_not_recognised() {
        let vars: Vec<_> = REQUIRED
            .iter()
            .map(|&(k, v)| {
                if k == "CARDIGAN_DATABASE_PATH" {
                    ("CARDIGAN__DATABASE_PATH", v)
                } else {
                    (k, v)
                }
            })
            .collect();
        let err = load(&vars).unwrap_err();
        assert_eq!(err.to_string(), "missing required environment variable(s): CARDIGAN_DATABASE_PATH");
    }

    #[test]
    fn numeric_looking_password_is_kept_verbatim() {
        let config = load(&with(&[("CARDIGAN_ICLOUD_PASSWORD", "007")])).unwrap();
        assert_eq!(config.icloud.password.expose(), "007");
    }

    #[test]
    fn poll_interval_at_minimum_is_accepted() {
        let config = load(&with(&[("CARDIGAN_POLL_INTERVAL_SECS", "60")])).unwrap();
        assert_eq!(config.poll_interval, Duration::from_secs(60));
    }

    #[test]
    fn poll_interval_below_minimum_is_rejected() {
        let err = load(&with(&[("CARDIGAN_POLL_INTERVAL_SECS", "59")])).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid value for CARDIGAN_POLL_INTERVAL_SECS: must be at least 60 seconds, got 59"
        );
    }

    #[test]
    fn non_numeric_poll_interval_is_rejected() {
        let err = load(&with(&[("CARDIGAN_POLL_INTERVAL_SECS", "soon")])).unwrap_err();
        assert_eq!(err.to_string(), "invalid value for CARDIGAN_POLL_INTERVAL_SECS: invalid digit found in string");
    }

    #[test]
    fn unknown_conflict_winner_is_rejected() {
        let err = load(&with(&[("CARDIGAN_CONFLICT_WINNER", "google")])).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid value for CARDIGAN_CONFLICT_WINNER: expected `icloud` or `fastmail`, got `google`"
        );
    }

    #[test]
    fn non_numeric_max_photo_bytes_is_rejected() {
        let err = load(&with(&[("CARDIGAN_MAX_PHOTO_BYTES", "1MiB")])).unwrap_err();
        assert_eq!(err.to_string(), "invalid value for CARDIGAN_MAX_PHOTO_BYTES: invalid digit found in string");
    }

    fn load_with_database_path(path: &str) -> Config {
        let mut vars: Vec<(&str, &str)> = REQUIRED.iter().filter(|(k, _)| *k != "CARDIGAN_DATABASE_PATH").copied().collect();
        vars.push(("CARDIGAN_DATABASE_PATH", path));
        load(&vars).unwrap()
    }

    #[test]
    fn database_url_points_at_cardigan_db_in_directory() {
        let dir = std::env::temp_dir();
        let config = load_with_database_path(dir.to_str().unwrap());
        assert_eq!(
            config.database_url().unwrap(),
            format!("sqlite://{}?mode=rwc", dir.join("cardigan.db").display())
        );
    }

    #[test]
    fn database_url_rejects_missing_directory() {
        let config = load_with_database_path("/definitely/not/a/cardigan/dir");
        assert_eq!(
            config.database_url().unwrap_err().to_string(),
            "invalid value for CARDIGAN_DATABASE_PATH: /definitely/not/a/cardigan/dir is not an existing directory"
        );
    }

    #[test]
    fn database_url_rejects_path_containing_question_mark() {
        let path = "/var/lib/cardigan?evil=1";
        let config = load_with_database_path(path);
        assert_eq!(
            config.database_url().unwrap_err().to_string(),
            "invalid value for CARDIGAN_DATABASE_PATH: /var/lib/cardigan?evil=1 contains '?' or '%', which the SQLite URL cannot represent"
        );
    }

    #[test]
    fn database_url_rejects_regular_file() {
        let path = std::env::temp_dir().join(format!("cardigan-test-file-{}", std::process::id()));
        std::fs::write(&path, b"not a directory").unwrap();

        let config = load_with_database_path(path.to_str().unwrap());
        assert_eq!(
            config.database_url().unwrap_err().to_string(),
            format!("invalid value for CARDIGAN_DATABASE_PATH: {} is not an existing directory", path.display())
        );

        std::fs::remove_file(&path).unwrap();
    }
}
