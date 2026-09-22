use serde::Deserialize;

use crate::error::Error;

#[derive(Debug, Deserialize)]
pub struct Config {}

impl Config {
    pub fn load() -> Result<Self, Error> {
        let config = config::Config::builder()
            .add_source(config::Environment::with_prefix("CARDIGAN").try_parsing(true).separator("__"))
            .build()?;

        let config = config.try_deserialize()?;

        Ok(config)
    }
}
