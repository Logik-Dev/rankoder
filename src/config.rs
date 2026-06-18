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
    pub min_bpp_hevc: f64,
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
    pub min_vmaf: f64,
    pub vmaf_n_subsample: u32,
    pub backfill_vmaf: bool,
    pub requeue_quality_skips: bool,
    pub radarr_url: Option<String>,
    pub radarr_api_key: Option<String>,
    pub sonarr_url: Option<String>,
    pub sonarr_api_key: Option<String>,
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
            // HEVC is already efficient, so only re-encode clearly over-bitrate
            // sources (remux-tier). Higher dedicated threshold, gated on bpp
            // alone (the compression_potential heuristic is tuned for h264).
            min_bpp_hevc: parse_env("MIN_ANALYSIS_BPP_HEVC", 0.15)?,
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
            // Post-encode quality gate. The VMAF score is always measured and
            // recorded; MIN_VMAF=0 (default) is "observe only" — measure but
            // never reject, so the threshold can be calibrated from real data.
            min_vmaf: parse_env("MIN_VMAF", 0.0)?,
            vmaf_n_subsample: parse_env("VMAF_N_SUBSAMPLE", 5)?,
            // One-shot maintenance flags, read at startup. Both are idempotent,
            // but documented as set -> run once -> unset.
            backfill_vmaf: parse_bool_env("BACKFILL_VMAF", false)?,
            requeue_quality_skips: parse_bool_env("REQUEUE_QUALITY_SKIPS", false)?,
            // Optional: when unset, no media-manager refresh is performed after
            // a transcode completes. Radarr handles movies, Sonarr series.
            radarr_url: env::var("RADARR_URL").ok(),
            radarr_api_key: env::var("RADARR_API_KEY").ok(),
            sonarr_url: env::var("SONARR_URL").ok(),
            sonarr_api_key: env::var("SONARR_API_KEY").ok(),
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
