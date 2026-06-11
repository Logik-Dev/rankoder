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

#[derive(Debug, Clone)]
pub struct AnalysisConfig {
    /// Minimum GB per hour of content — normalises episodes vs movies (MIN_ANALYSIS_SIZE_PER_HOUR_GB, default 2.0)
    pub min_size_per_hour_gb: f64,
    /// Minimum bits-per-pixel threshold — files below are already compressed (MIN_ANALYSIS_BPP, default 0.04)
    pub min_bpp: f64,
    /// Minimum compression potential score to trigger encoding (MIN_COMPRESSION_POTENTIAL, default 1.0)
    pub min_compression_potential: f64,
}

impl AnalysisConfig {
    pub fn from_env() -> Self {
        Self {
            min_size_per_hour_gb: parse_env("MIN_ANALYSIS_SIZE_PER_HOUR_GB", 2.0),
            min_bpp: parse_env("MIN_ANALYSIS_BPP", 0.04),
            min_compression_potential: parse_env("MIN_COMPRESSION_POTENTIAL", 1.0),
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
