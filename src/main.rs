mod config;
mod genre;
mod links;
mod matching;
mod matrix;
mod model;
mod providers;
mod translate;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: tunebridge [CONFIG]            run the bot
       tunebridge translate TEXT...   print the reply for links in TEXT (no Matrix login)";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("tunebridge=info,matrix_sdk=warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("translate") => translate_cli(&args[1..]).await,
        _ => {
            let path = config::Config::locate();
            let cfg = config::Config::load(&path)?;
            tracing::info!(config = %path.display(), version = env!("CARGO_PKG_VERSION"), "starting tunebridge");
            matrix::run(cfg).await
        }
    }
}

/// Debug helper: resolve and translate links without touching Matrix.
/// Config comes from `TUNEBRIDGE_CONFIG` or `./config.toml`.
async fn translate_cli(texts: &[String]) -> Result<()> {
    anyhow::ensure!(!texts.is_empty(), "{USAGE}");
    let cfg = config::Config::load(&config::Config::locate())?;
    let translator = translate::Translator::new(&cfg)?;
    let text = texts.join(" ");
    let replies = translator.translate_message(&text, None).await;
    match translate::format_replies(&replies, true) {
        Some((plain, html)) => {
            println!("{plain}\n\n--- html ---\n{html}");
        }
        None => println!("no music links found"),
    }
    Ok(())
}
