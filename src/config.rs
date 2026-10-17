//! TOML configuration with environment overrides for secrets.

use crate::model::Service;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub matrix: MatrixConfig,
    pub spotify: Option<SpotifyConfig>,
    pub youtube: Option<YoutubeConfig>,
    #[serde(default)]
    pub behaviour: Behaviour,
}

#[derive(Debug, Deserialize)]
pub struct MatrixConfig {
    pub homeserver: String,
    pub user_id: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_device_name")]
    pub device_name: String,
    /// Empty = respond in every joined room.
    #[serde(default)]
    pub allowed_rooms: Vec<String>,
    /// Users whose invites are accepted automatically. Empty = accept none.
    #[serde(default)]
    pub auto_join_invites_from: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SpotifyConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct YoutubeConfig {
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
pub struct Behaviour {
    /// Services to translate *to*. The source service is always skipped.
    #[serde(default = "default_targets")]
    pub targets: Vec<Service>,
    #[serde(default)]
    pub translate_plain_youtube: bool,
    #[serde(default)]
    pub reply_on_miss: bool,
    #[serde(default = "default_max_links")]
    pub max_links_per_message: usize,
    /// Append genre emoji (e.g. 🤘🎸 for rock) when a genre is known.
    #[serde(default = "default_true")]
    pub genre_emojis: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            targets: default_targets(),
            translate_plain_youtube: false,
            reply_on_miss: false,
            max_links_per_message: default_max_links(),
            genre_emojis: true,
        }
    }
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}
fn default_device_name() -> String {
    "tunebridge".to_string()
}
fn default_targets() -> Vec<Service> {
    vec![Service::Spotify, Service::YoutubeMusic]
}
fn default_max_links() -> usize {
    5
}
fn default_true() -> bool {
    true
}

impl Config {
    /// Path from `TUNEBRIDGE_CONFIG`, else the first CLI argument, else `./config.toml`.
    pub fn locate() -> PathBuf {
        if let Ok(p) = std::env::var("TUNEBRIDGE_CONFIG") {
            return PathBuf::from(p);
        }
        if let Some(p) = std::env::args().nth(1) {
            return PathBuf::from(p);
        }
        PathBuf::from("config.toml")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        cfg.apply_env();
        cfg.validate()?;
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("TUNEBRIDGE_MATRIX_PASSWORD") {
            self.matrix.password = v;
        }
        let sp_id = std::env::var("TUNEBRIDGE_SPOTIFY_CLIENT_ID").ok();
        let sp_secret = std::env::var("TUNEBRIDGE_SPOTIFY_CLIENT_SECRET").ok();
        match (&mut self.spotify, sp_id, sp_secret) {
            (Some(s), id, secret) => {
                if let Some(id) = id {
                    s.client_id = id;
                }
                if let Some(secret) = secret {
                    s.client_secret = secret;
                }
            }
            (slot @ None, Some(id), Some(secret)) => {
                *slot = Some(SpotifyConfig {
                    client_id: id,
                    client_secret: secret,
                });
            }
            _ => {}
        }
        if let Ok(key) = std::env::var("TUNEBRIDGE_YOUTUBE_API_KEY") {
            self.youtube = Some(YoutubeConfig { api_key: key });
        }
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.matrix.homeserver.is_empty(),
            "matrix.homeserver is required"
        );
        anyhow::ensure!(
            self.matrix.user_id.starts_with('@'),
            "matrix.user_id must look like @user:server"
        );
        if self.spotify.is_none() {
            tracing::warn!(
                "no [spotify] credentials: Spotify links cannot be resolved or produced"
            );
        }
        if self.youtube.is_none() {
            tracing::warn!(
                "no [youtube] credentials: YouTube Music links cannot be resolved or produced"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_parses_with_defaults() {
        let cfg: Config = toml::from_str(
            r#"
            [matrix]
            homeserver = "https://matrix.example"
            user_id = "@bot:example"
            [spotify]
            client_id = "a"
            client_secret = "b"
            [youtube]
            api_key = "k"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.behaviour.targets,
            vec![Service::Spotify, Service::YoutubeMusic]
        );
        assert!(!cfg.behaviour.translate_plain_youtube);
        assert_eq!(cfg.behaviour.max_links_per_message, 5);
        assert!(cfg.behaviour.genre_emojis);
        assert_eq!(cfg.matrix.data_dir, PathBuf::from("./data"));
    }
}
