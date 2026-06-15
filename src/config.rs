use std::env;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required environment variable: {0}")]
    Missing(String),
    #[error("invalid value for {key}: '{value}' — {reason}")]
    InvalidValue {
        key: String,
        value: String,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub jellyfin_url: String,
    pub jellyfin_api_key: String,
    pub database_url: String,
    pub auto_migrate: bool,
    pub min_size_per_hour_gb: f64,
    pub min_bpp: f64,
    pub min_compression_potential: f64,
    pub mqtt_host: String,
    pub mqtt_port: u16,
    pub mqtt_client_id: String,
    pub approval_max_pending: usize,
    pub transcode_tmp_dir: String,
    pub transcode_retention_dir: String,
    pub transcode_encoder_override: String,
    pub transcode_min_size_reduction: f64,
    pub transcode_retention_days: i32,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            jellyfin_url: env::var("JELLYFIN_URL")
                .map_err(|_| ConfigError::Missing("JELLYFIN_URL".into()))?,
            jellyfin_api_key: env::var("JELLYFIN_API_KEY")
                .map_err(|_| ConfigError::Missing("JELLYFIN_API_KEY".into()))?,
            database_url: env::var("DATABASE_URL")
                .map_err(|_| ConfigError::Missing("DATABASE_URL".into()))?,
            auto_migrate: parse_bool_env("AUTO_MIGRATE", false)?,
            min_size_per_hour_gb: parse_env("MIN_ANALYSIS_SIZE_PER_HOUR_GB", 2.0)?,
            min_bpp: parse_env("MIN_ANALYSIS_BPP", 0.04)?,
            min_compression_potential: parse_env("MIN_COMPRESSION_POTENTIAL", 1.0)?,
            mqtt_host: env::var("MQTT_HOST")
                .map_err(|_| ConfigError::Missing("MQTT_HOST".into()))?,
            mqtt_port: parse_env("MQTT_PORT", 1883)?,
            mqtt_client_id: parse_env("MQTT_CLIENT_ID", "rankoder".to_string())?,
            approval_max_pending: parse_env("APPROVAL_MAX_PENDING", 2)?,
            transcode_tmp_dir: env::var("TRANSCODE_TMP_DIR")
                .map_err(|_| ConfigError::Missing("TRANSCODE_TMP_DIR".into()))?,
            transcode_retention_dir: env::var("TRANSCODE_RETENTION_DIR")
                .map_err(|_| ConfigError::Missing("TRANSCODE_RETENTION_DIR".into()))?,
            transcode_encoder_override: parse_env("TRANSCODE_ENCODER", "auto".to_string())?,
            transcode_min_size_reduction: parse_env("TRANSCODE_MIN_SIZE_REDUCTION", 0.1)?,
            transcode_retention_days: parse_env("TRANSCODE_RETENTION_DAYS", 7)?,
        })
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> Result<T, ConfigError>
where
    T::Err: std::fmt::Display,
{
    match env::var(key) {
        Ok(v) => v.parse().map_err(|e: T::Err| ConfigError::InvalidValue {
            key: key.into(),
            value: v,
            reason: e.to_string(),
        }),
        Err(_) => Ok(default),
    }
}

fn parse_bool_env(key: &str, default: bool) -> Result<bool, ConfigError> {
    match env::var(key) {
        Ok(v) => match v.as_str() {
            "1" => Ok(true),
            "0" => Ok(false),
            _ => Err(ConfigError::InvalidValue {
                key: key.into(),
                value: v,
                reason: "expected 1 or 0".into(),
            }),
        },
        Err(_) => Ok(default),
    }
}
