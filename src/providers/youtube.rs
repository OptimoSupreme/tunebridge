//! YouTube Data API v3. YouTube Music has no public API, but every YTM song is
//! a YouTube video and every YTM album is an auto-generated `OLAK5uy_` playlist,
//! so the Data API covers both directions.

use crate::config::YoutubeConfig;
use crate::links;
use crate::matching::{self, channel_to_artist, split_artist_title};
use crate::model::{Candidate, Item, Kind, MusicRef, Service};
use anyhow::{Context, Result, anyhow};
use regex::Regex;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::sync::LazyLock;

const API: &str = "https://www.googleapis.com/youtube/v3";

pub struct YouTube {
    http: reqwest::Client,
    api_key: String,
}

#[derive(Deserialize)]
struct ListResp<T> {
    #[serde(default = "Vec::new")]
    items: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snippet {
    title: String,
    #[serde(default)]
    channel_title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Video {
    id: String,
    snippet: Snippet,
    content_details: Option<ContentDetails>,
    topic_details: Option<TopicDetails>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TopicDetails {
    #[serde(default)]
    topic_categories: Vec<String>,
}

#[derive(Deserialize)]
struct ContentDetails {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct Playlist {
    snippet: Snippet,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchId {
    video_id: Option<String>,
    playlist_id: Option<String>,
}

#[derive(Deserialize)]
struct SearchItem {
    id: SearchId,
    snippet: Snippet,
}

static OLAK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"OLAK5uy_[A-Za-z0-9_-]{10,}").expect("olak regex"));

impl YouTube {
    pub fn new(http: reqwest::Client, cfg: &YoutubeConfig) -> Self {
        Self {
            http,
            api_key: cfg.api_key.clone(),
        }
    }

    async fn get<T: DeserializeOwned>(&self, resource: &str, query: &[(&str, &str)]) -> Result<T> {
        let resp = self
            .http
            .get(format!("{API}/{resource}"))
            .query(query)
            .query(&[("key", self.api_key.as_str())])
            .send()
            .await
            .with_context(|| format!("youtube GET {resource}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let hint = if body.contains("quotaExceeded") {
                " (daily quota exhausted; resets at midnight Pacific)"
            } else {
                ""
            };
            return Err(anyhow!(
                "youtube {resource} returned {status}{hint}: {}",
                body.chars().take(200).collect::<String>()
            ));
        }
        resp.json::<T>()
            .await
            .with_context(|| format!("youtube {resource} json"))
    }

    pub async fn resolve(&self, r: &MusicRef) -> Result<Item> {
        match r.kind {
            Kind::Track => {
                let resp: ListResp<Video> = self
                    .get(
                        "videos",
                        &[
                            ("part", "snippet,contentDetails,topicDetails"),
                            ("id", &r.id),
                        ],
                    )
                    .await?;
                let v = resp
                    .items
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("youtube video {} not found", r.id))?;
                let (channel_artist, is_topic) = channel_to_artist(&v.snippet.channel_title);
                let (artist, title) =
                    split_artist_title(&v.snippet.title, &channel_artist, is_topic);
                Ok(Item {
                    source: r.clone(),
                    title,
                    artists: vec![artist],
                    duration_ms: v
                        .content_details
                        .and_then(|c| c.duration)
                        .and_then(|d| parse_iso8601_ms(&d)),
                    isrc: None,
                    genres: v
                        .topic_details
                        .map(|t| t.topic_categories)
                        .unwrap_or_default(),
                })
            }
            Kind::Album => {
                let resp: ListResp<Playlist> = self
                    .get("playlists", &[("part", "snippet"), ("id", &r.id)])
                    .await?;
                let p = resp
                    .items
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("youtube playlist {} not found", r.id))?;
                let (artist, _) = channel_to_artist(&p.snippet.channel_title);
                Ok(Item {
                    source: r.clone(),
                    title: matching::clean_for_query(&p.snippet.title),
                    artists: vec![artist],
                    duration_ms: None,
                    isrc: None,
                    genres: Vec::new(),
                })
            }
        }
    }

    pub async fn search(&self, item: &Item) -> Result<Vec<Candidate>> {
        let title = matching::clean_for_query(&item.title);
        let artist = item.primary_artist();
        match item.kind() {
            Kind::Track => {
                let q = format!("{artist} {title}");
                tracing::debug!(%q, "youtube video search");
                let s: ListResp<SearchItem> = self
                    .get(
                        "search",
                        &[
                            ("part", "snippet"),
                            ("type", "video"),
                            ("videoCategoryId", "10"),
                            ("maxResults", "6"),
                            ("q", &q),
                        ],
                    )
                    .await?;
                let ids: Vec<String> = s.items.into_iter().filter_map(|i| i.id.video_id).collect();
                if ids.is_empty() {
                    return Ok(Vec::new());
                }
                // Second call (1 quota unit) for exact durations and un-escaped titles.
                let v: ListResp<Video> = self
                    .get(
                        "videos",
                        &[
                            ("part", "snippet,contentDetails,topicDetails"),
                            ("id", &ids.join(",")),
                        ],
                    )
                    .await?;
                Ok(v.items
                    .into_iter()
                    .map(|v| {
                        let (channel_artist, is_topic) =
                            channel_to_artist(&v.snippet.channel_title);
                        Candidate {
                            music_ref: MusicRef {
                                service: Service::YoutubeMusic,
                                kind: Kind::Track,
                                id: v.id,
                            },
                            title: v.snippet.title,
                            artists: vec![channel_artist],
                            duration_ms: v
                                .content_details
                                .and_then(|c| c.duration)
                                .and_then(|d| parse_iso8601_ms(&d)),
                            bonus: if is_topic { 0.05 } else { 0.0 },
                            genres: v
                                .topic_details
                                .map(|t| t.topic_categories)
                                .unwrap_or_default(),
                        }
                    })
                    .collect())
            }
            Kind::Album => {
                let q = format!("{artist} {title} album");
                tracing::debug!(%q, "youtube playlist search");
                let s: ListResp<SearchItem> = self
                    .get(
                        "search",
                        &[
                            ("part", "snippet"),
                            ("type", "playlist"),
                            ("maxResults", "8"),
                            ("q", &q),
                        ],
                    )
                    .await?;
                Ok(s.items
                    .into_iter()
                    .filter_map(|i| {
                        let id = i.id.playlist_id?;
                        let (channel_artist, is_topic) =
                            channel_to_artist(&i.snippet.channel_title);
                        let is_album = id.starts_with("OLAK5uy_");
                        Some(Candidate {
                            music_ref: MusicRef {
                                service: Service::YoutubeMusic,
                                kind: Kind::Album,
                                id,
                            },
                            title: links::unescape_html(&i.snippet.title),
                            artists: vec![channel_artist],
                            duration_ms: None,
                            genres: Vec::new(),
                            bonus: (if is_album { 0.08 } else { -0.15 })
                                + (if is_topic { 0.03 } else { 0.0 }),
                        })
                    })
                    .collect())
            }
        }
    }

    /// `music.youtube.com/browse/MPREb_...` album pages embed the matching
    /// `OLAK5uy_` playlist id in their HTML. Best effort, no API quota used.
    pub async fn browse_to_playlist(&self, browse_id: &str) -> Result<Option<String>> {
        let html = self
            .http
            .get(format!("https://music.youtube.com/browse/{browse_id}"))
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header(reqwest::header::COOKIE, "SOCS=CAI; CONSENT=YES+")
            .send()
            .await
            .context("youtube music browse page")?
            .text()
            .await?;
        Ok(OLAK_RE.find(&html).map(|m| m.as_str().to_string()))
    }
}

/// `PT3M20S` -> 200000. Handles the D/H/M/S designators YouTube emits.
pub fn parse_iso8601_ms(s: &str) -> Option<u64> {
    let s = s.strip_prefix('P')?;
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut secs = 0u64;
    let mut num = String::new();
    for c in date.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: u64 = num.parse().ok()?;
            num.clear();
            match c {
                'W' => secs += n * 604_800,
                'D' => secs += n * 86_400,
                _ => return None,
            }
        }
    }
    for c in time.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: u64 = num.parse().ok()?;
            num.clear();
            match c {
                'H' => secs += n * 3_600,
                'M' => secs += n * 60,
                'S' => secs += n,
                _ => return None,
            }
        }
    }
    Some(secs * 1000)
}

#[cfg(test)]
mod tests {
    use super::parse_iso8601_ms;

    #[test]
    fn durations() {
        assert_eq!(parse_iso8601_ms("PT3M20S"), Some(200_000));
        assert_eq!(parse_iso8601_ms("PT1H2M3S"), Some(3_723_000));
        assert_eq!(parse_iso8601_ms("PT45S"), Some(45_000));
        assert_eq!(parse_iso8601_ms("P1DT1S"), Some(86_401_000));
        assert_eq!(parse_iso8601_ms("PT0S"), Some(0));
        assert_eq!(parse_iso8601_ms("3:20"), None);
    }
}
