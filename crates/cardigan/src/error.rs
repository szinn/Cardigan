use config::ConfigError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Any(#[from] Box<dyn std::error::Error + Send + Sync>),

    #[error(transparent)]
    ConfigError(#[from] ConfigError),

    #[error("missing required environment variable(s): {}", .0.join(", "))]
    MissingVariables(Vec<&'static str>),

    #[error("invalid value for {variable}: {reason}")]
    InvalidValue { variable: &'static str, reason: String },
}
