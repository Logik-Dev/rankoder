use std::env;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Config {
    pub jellyfin_url: String,
    pub jellyfin_api_key: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let jellyfin_url = env::var("JELLYFIN_URL")?;
        let jellyfin_api_key = env::var("JELLYFIN_API_KEY")?;

        Ok(Self {
            jellyfin_url,
            jellyfin_api_key,
        })
    }
}
