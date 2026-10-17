mod config;
mod links;
mod matching;
mod matrix;
mod model;
mod providers;
mod translate;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("tunebridge=info,matrix_sdk=warn")),
        )
        .init();

    let path = config::Config::locate();
    let cfg = config::Config::load(&path)?;
    tracing::info!(config = %path.display(), version = env!("CARGO_PKG_VERSION"), "starting tunebridge");
    matrix::run(cfg).await
}
