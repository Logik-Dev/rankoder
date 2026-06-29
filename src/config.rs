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
    pub mqtt_username: Option<String>,
    pub mqtt_password: Option<String>,
    pub approval_max_pending: usize,
    pub transcode_tmp_dir: String,
    pub transcode_retention_dir: String,
    pub transcode_encoder_override: String,
    pub transcode_min_size_reduction: f64,
    pub transcode_retention_days: i32,
    pub min_vmaf: f64,
    pub vmaf_n_subsample: u32,
    pub vmaf_n_threads: usize,
    pub backfill_vmaf: bool,
    pub requeue_quality_skips: bool,
    /// Periodic library re-sync cadence, in seconds. `0` disables the timer
    /// (sync then only runs at startup and on external triggers).
    pub sync_interval_secs: u64,
    /// Debounce window (seconds) applied to webhook/MQTT triggers so a burst of
    /// events (e.g. importing a whole season) collapses into a single sync.
    pub sync_debounce_secs: u64,
    /// `host:port` to bind the HTTP server (operator UI + sync webhook). Unset
    /// (or empty) disables the server entirely. Bind loopback and front it with
    /// a reverse proxy — the UI has no auth of its own.
    pub http_bind: Option<String>,
    /// Shared secret expected in the `X-Rankoder-Token` header on webhook calls.
    /// Optional: when unset the `/sync` webhook is simply not mounted (the UI is
    /// still served). The webhook is opt-in by the token's presence.
    pub webhook_token: Option<String>,
    /// Shared secret gating the UI's mutating actions (`/actions/*`, e.g. the
    /// failure requeue). Optional: when unset those routes are not mounted at
    /// all and the dashboard stays strictly read-only. When set, the server
    /// embeds it as a hidden field in each action form and checks it on POST —
    /// defence in depth on top of the reverse proxy, and a same-origin guard
    /// (a cross-origin page can't read the token to forge the request).
    pub ui_control_token: Option<String>,
    pub radarr_url: Option<String>,
    pub radarr_api_key: Option<String>,
    pub sonarr_url: Option<String>,
    pub sonarr_api_key: Option<String>,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let config = Self {
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
            mqtt_username: non_empty(env::var("MQTT_USERNAME").ok()),
            mqtt_password: non_empty(env::var("MQTT_PASSWORD").ok()),
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
            // libvmaf is single-threaded by default and dominates the measure
            // cost; threading it is a ~3x win. Capped (default 6) so it leaves
            // cores free for the host rather than grabbing every CPU.
            vmaf_n_threads: parse_env("VMAF_N_THREADS", 6)?,
            // One-shot maintenance flags, read at startup. Both are idempotent,
            // but documented as set -> run once -> unset.
            backfill_vmaf: parse_bool_env("BACKFILL_VMAF", false)?,
            requeue_quality_skips: parse_bool_env("REQUEUE_QUALITY_SKIPS", false)?,
            // Library re-sync: periodic safety net + debounced external triggers.
            sync_interval_secs: parse_env("SYNC_INTERVAL_SECS", 3600u64)?,
            sync_debounce_secs: parse_env("SYNC_DEBOUNCE_SECS", 15u64)?,
            // HTTP server is opt-in: enabled only when a bind address is given.
            // It serves the operator UI; the `/sync` webhook is mounted on top
            // only when WEBHOOK_TOKEN is also set (see http::serve).
            http_bind: non_empty(env::var("HTTP_BIND").ok()),
            webhook_token: non_empty(env::var("WEBHOOK_TOKEN").ok()),
            // UI write actions are opt-in: mounted only when this is set.
            ui_control_token: non_empty(env::var("UI_CONTROL_TOKEN").ok()),
            // Optional: when unset, no media-manager refresh is performed after
            // a transcode completes. Radarr handles movies, Sonarr series.
            radarr_url: env::var("RADARR_URL").ok(),
            radarr_api_key: env::var("RADARR_API_KEY").ok(),
            sonarr_url: env::var("SONARR_URL").ok(),
            sonarr_api_key: env::var("SONARR_API_KEY").ok(),
        };

        Ok(config)
    }
}

/// Treats empty strings as absent, so an env var set to "" disables the feature
/// instead of becoming a degenerate value.
fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
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
