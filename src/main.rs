use std::sync::Arc;

use tracing::instrument;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    config::Config,
    providers::{JellyfinProvider, MovieProvider, SeriesProvider},
};

mod config;
pub mod models;
pub mod providers;

#[tokio::main]
#[instrument(err)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let cfg = Config::from_env()?;

    let provider = Arc::new(JellyfinProvider::new(
        &cfg.jellyfin_url,
        &cfg.jellyfin_api_key,
    )?);

    let series_provider: Arc<dyn SeriesProvider> = provider.clone();
    let series = series_provider.list_series().await?;
    for serie in &series {
        println!("{:?}", serie);
        let episodes = series_provider.list_episodes(serie).await?;
        for ep in &episodes {
            println!("  {:?}", ep);
        }
    }

    let movie_provider: Arc<dyn MovieProvider> = provider;
    let movies = movie_provider.list_movies().await?;
    for movie in &movies {
        println!("{:?}", movie);
    }

    Ok(())
}

// Logging file + stdout
fn init_tracing() {
    let stdout_layer = fmt::layer().compact().with_target(false);

    let file_appender = tracing_appender::rolling::daily("logs", "rankoder.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // Leak the guard so the background writer thread lives for the entire process.
    std::mem::forget(guard);
    let json_layer = fmt::layer().json().with_writer(non_blocking);

    let _ = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(stdout_layer)
        .with(json_layer)
        .try_init();
}
