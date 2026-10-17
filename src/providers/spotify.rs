//! Spotify Web API via the client-credentials flow (no user login needed).

use crate::config::SpotifyConfig;
use crate::links::{self, Found, Options};
use crate::matching;
use crate::model::{Candidate, Item, Kind, MusicRef, Service};
use anyhow::{Context, Result, anyhow};
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const API: &str = "https://api.spotify.com/v1";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

pub struct Spotify {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    token: Mutex<Option<Token>>,
}

struct Token {
    value: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct TrackJson {
    id: String,
    name: String,
    #[serde(default)]
    artists: Vec<Named>,
    duration_ms: u64,
    external_ids: Option<ExternalIds>,
}

#[derive(Deserialize)]
struct ExternalIds {
    isrc: Option<String>,
}

#[derive(Deserialize)]
struct AlbumJson {
    id: String,
    name: String,
    #[serde(default)]
    artists: Vec<Named>,
}

/// Spotify search pages may contain `null` entries.
#[derive(Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    items: Vec<Option<T>>,
}

#[derive(Deserialize)]
struct SearchResp {
    tracks: Option<Page<TrackJson>>,
    albums: Option<Page<AlbumJson>>,
}

impl Spotify {
    pub fn new(http: reqwest::Client, cfg: &SpotifyConfig) -> Self {
        Self {
            http,
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.clone(),
            token: Mutex::new(None),
        }
    }

    async fn token(&self) -> Result<String> {
        let mut guard = self.token.lock().await;
        if let Some(t) = guard.as_ref()
            && t.expires_at > Instant::now()
        {
            return Ok(t.value.clone());
        }
        let resp = self
            .http
            .post(TOKEN_URL)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .context("spotify token request")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "spotify token endpoint returned {status}: {}",
                snippet(&body)
            ));
        }
        let t: TokenResp = resp.json().await.context("spotify token json")?;
        tracing::debug!(expires_in = t.expires_in, "fetched spotify access token");
        *guard = Some(Token {
            value: t.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(t.expires_in.saturating_sub(60)),
        });
        Ok(t.access_token)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let resp = self
                .http
                .get(format!("{API}/{path}"))
                .bearer_auth(token)
                .query(query)
                .send()
                .await
                .with_context(|| format!("spotify GET {path}"))?;
            let status = resp.status();
            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                tracing::debug!("spotify token rejected, refreshing");
                self.token.lock().await.take();
                continue;
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "spotify {path} returned {status}: {}",
                    snippet(&body)
                ));
            }
            return resp
                .json::<T>()
                .await
                .with_context(|| format!("spotify {path} json"));
        }
        unreachable!("loop returns on second attempt")
    }

    pub async fn resolve(&self, r: &MusicRef) -> Result<Item> {
        match r.kind {
            Kind::Track => {
                let t: TrackJson = self.get(&format!("tracks/{}", r.id), &[]).await?;
                Ok(Item {
                    source: r.clone(),
                    title: t.name,
                    artists: t.artists.into_iter().map(|a| a.name).collect(),
                    duration_ms: Some(t.duration_ms),
                    isrc: t.external_ids.and_then(|e| e.isrc),
                    duration_reliable: true,
                    // Spotify no longer exposes genres to new apps (artist objects come
                    // back without the field; /v1/artists?ids= is 403), so genre tags
                    // come from the YouTube side only.
                    genres: Vec::new(),
                })
            }
            Kind::Album => {
                let a: AlbumJson = self.get(&format!("albums/{}", r.id), &[]).await?;
                Ok(Item {
                    source: r.clone(),
                    title: a.name,
                    artists: a.artists.into_iter().map(|a| a.name).collect(),
                    duration_ms: None,
                    isrc: None,
                    duration_reliable: true,
                    // Spotify no longer exposes genres to new apps (artist objects come
                    // back without the field; /v1/artists?ids= is 403), so genre tags
                    // come from the YouTube side only.
                    genres: Vec::new(),
                })
            }
        }
    }

    /// Progressively looser queries; stops as soon as one yields an acceptable match.
    pub async fn search(&self, item: &Item) -> Result<Vec<Candidate>> {
        let title = matching::clean_for_query(&item.title);
        let artist = item.primary_artist();
        let (type_, queries) = match item.kind() {
            Kind::Track => {
                let mut q = Vec::new();
                if let Some(isrc) = &item.isrc {
                    q.push(format!("isrc:{isrc}"));
                }
                q.push(format!("track:{title} artist:{artist}"));
                q.push(format!("{title} {artist}"));
                ("track", q)
            }
            Kind::Album => (
                "album",
                vec![
                    format!("album:{title} artist:{artist}"),
                    format!("{title} {artist}"),
                ],
            ),
        };

        let mut out: Vec<Candidate> = Vec::new();
        for q in queries {
            tracing::debug!(%q, "spotify search");
            let resp: SearchResp = self
                .get("search", &[("q", &q), ("type", type_), ("limit", "5")])
                .await?;
            let found: Vec<Candidate> = match item.kind() {
                Kind::Track => resp
                    .tracks
                    .map(|p| p.items.into_iter().flatten().map(track_candidate).collect())
                    .unwrap_or_default(),
                Kind::Album => resp
                    .albums
                    .map(|p| p.items.into_iter().flatten().map(album_candidate).collect())
                    .unwrap_or_default(),
            };
            for c in found {
                if !out.iter().any(|o| o.music_ref == c.music_ref) {
                    out.push(c);
                }
            }
            if matching::best(item, &out).is_some() {
                break;
            }
        }
        Ok(out)
    }

    /// Follow a `spotify.link/...` redirect chain to a canonical track/album URL.
    pub async fn expand_short_link(&self, url: &str) -> Result<Option<MusicRef>> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .context("spotify.link redirect")?;
        let final_url = resp.url().to_string();
        tracing::debug!(%url, %final_url, "expanded spotify short link");
        Ok(match links::classify_url(&final_url, Options::default()) {
            Some(Found::Ref(r)) => Some(r),
            _ => None,
        })
    }
}

fn track_candidate(t: TrackJson) -> Candidate {
    Candidate {
        music_ref: MusicRef {
            service: Service::Spotify,
            kind: Kind::Track,
            id: t.id,
        },
        title: t.name,
        artists: t.artists.into_iter().map(|a| a.name).collect(),
        duration_ms: Some(t.duration_ms),
        bonus: 0.0,
        duration_reliable: true,
        genres: Vec::new(),
    }
}

fn album_candidate(a: AlbumJson) -> Candidate {
    Candidate {
        music_ref: MusicRef {
            service: Service::Spotify,
            kind: Kind::Album,
            id: a.id,
        },
        title: a.name,
        artists: a.artists.into_iter().map(|a| a.name).collect(),
        duration_ms: None,
        bonus: 0.0,
        duration_reliable: true,
        genres: Vec::new(),
    }
}

fn snippet(body: &str) -> String {
    body.chars().take(200).collect()
}
