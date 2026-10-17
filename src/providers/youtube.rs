//! YouTube Data API v3, plus YouTube Music's internal search for albums.
//!
//! Every YTM song is a YouTube video, so tracks go through the Data API
//! (`search.list` + `videos.list`). YTM albums are auto-generated `OLAK5uy_`
//! playlists that the Data API's search and `playlists.list(channelId=...)`
//! simply never return, so album *search* uses the same keyless InnerTube
//! endpoint the YouTube Music web app uses (what ytmusicapi has built on for
//! years). Resolving an `OLAK5uy_` link still works through the Data API.
//!
//! Quota notes (2026): `search.list` is capped at 100 calls/day in its own
//! bucket; `videos.list` / `playlists.list` / `playlistItems.list` cost 1 unit
//! each from 10 000/day, and adding `part`s to a call is free.

use crate::config::YoutubeConfig;
use crate::matching::{self, channel_to_artist, split_artist_title};
use crate::model::{Candidate, Item, Kind, MusicRef, Service};
use anyhow::{Context, Result, anyhow};
use regex::Regex;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::LazyLock;

const API: &str = "https://www.googleapis.com/youtube/v3";
const INNERTUBE_SEARCH: &str = "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";
/// InnerTube search filter: albums only.
const INNERTUBE_ALBUM_FILTER: &str = "EgWKAQIYAWoMEA4QChADEAQQCRAF";
const INNERTUBE_CLIENT_VERSION: &str = "1.20241028.01.00";

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
    /// Only on playlistItems: the channel that owns the video.
    #[serde(default)]
    video_owner_channel_title: String,
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
struct ContentDetails {
    duration: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TopicDetails {
    #[serde(default)]
    topic_categories: Vec<String>,
}

#[derive(Deserialize)]
struct WithSnippet {
    snippet: Snippet,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchId {
    video_id: Option<String>,
}

#[derive(Deserialize)]
struct SearchItem {
    id: SearchId,
}

static OLAK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"OLAK5uy_[A-Za-z0-9_-]{10,}").expect("olak regex"));

fn is_album_playlist(id: &str) -> bool {
    id.starts_with("OLAK5uy_")
}

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

    async fn videos(&self, ids: &[String]) -> Result<Vec<Video>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let resp: ListResp<Video> = self
            .get(
                "videos",
                &[
                    ("part", "snippet,contentDetails,topicDetails"),
                    ("id", &ids.join(",")),
                ],
            )
            .await?;
        Ok(resp.items)
    }

    pub async fn resolve(&self, r: &MusicRef) -> Result<Item> {
        match r.kind {
            Kind::Track => {
                let v = self
                    .videos(std::slice::from_ref(&r.id))
                    .await?
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
                    duration_ms: duration_of(&v),
                    isrc: None,
                    genres: topics_of(v.topic_details),
                    duration_reliable: is_topic,
                })
            }
            Kind::Album => {
                // Auto-generated album playlists are titled "Album - <name>" and
                // owned by a generic "YouTube" channel; the artist is on the tracks.
                let resp: ListResp<WithSnippet> = self
                    .get("playlists", &[("part", "snippet"), ("id", &r.id)])
                    .await?;
                let p = resp
                    .items
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("youtube playlist {} not found", r.id))?;
                let title = strip_album_prefix(&p.snippet.title);
                let first: ListResp<WithSnippet> = self
                    .get(
                        "playlistItems",
                        &[
                            ("part", "snippet"),
                            ("playlistId", &r.id),
                            ("maxResults", "1"),
                        ],
                    )
                    .await?;
                let owner = first
                    .items
                    .first()
                    .map(|i| i.snippet.video_owner_channel_title.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&p.snippet.channel_title);
                let (artist, _) = channel_to_artist(owner);
                let artists = if artist.eq_ignore_ascii_case("youtube") {
                    vec![]
                } else {
                    vec![artist]
                };
                Ok(Item {
                    source: r.clone(),
                    title: matching::clean_for_query(&title),
                    artists,
                    duration_ms: None,
                    isrc: None,
                    genres: Vec::new(),
                    duration_reliable: true,
                })
            }
        }
    }

    pub async fn search(&self, item: &Item) -> Result<Vec<Candidate>> {
        match item.kind() {
            Kind::Track => self.search_track(item).await,
            Kind::Album => self.search_album(item).await,
        }
    }

    async fn search_track(&self, item: &Item) -> Result<Vec<Candidate>> {
        let title = matching::clean_for_query(&item.title);
        let q = format!("{} {title}", item.primary_artist());
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
        // Second call (1 unit) for exact durations, topics and un-escaped titles.
        Ok(self
            .videos(&ids)
            .await?
            .into_iter()
            .map(video_candidate)
            .collect())
    }

    /// Album search through YouTube Music's own (keyless) search endpoint,
    /// first with the albums filter, then unfiltered if that yields nothing.
    async fn search_album(&self, item: &Item) -> Result<Vec<Candidate>> {
        let q = format!(
            "{} {}",
            item.primary_artist(),
            matching::clean_for_query(&item.title)
        );
        tracing::debug!(%q, "youtube music album search");
        let mut out = self
            .innertube_albums(&q, Some(INNERTUBE_ALBUM_FILTER))
            .await?;
        if out.is_empty() {
            tracing::debug!("filtered album search empty, retrying unfiltered");
            out = self.innertube_albums(&q, None).await?;
        }
        Ok(out)
    }

    async fn innertube_albums(&self, query: &str, params: Option<&str>) -> Result<Vec<Candidate>> {
        let mut body = json!({
            "context": {"client": {"clientName": "WEB_REMIX", "clientVersion": INNERTUBE_CLIENT_VERSION, "hl": "en", "gl": "US"}},
            "query": query,
        });
        if let Some(p) = params {
            body["params"] = Value::String(p.to_string());
        }
        let resp = self
            .http
            .post(INNERTUBE_SEARCH)
            .header(reqwest::header::ORIGIN, "https://music.youtube.com")
            .header(reqwest::header::REFERER, "https://music.youtube.com/")
            .header(reqwest::header::COOKIE, "SOCS=CAI; CONSENT=YES+")
            .json(&body)
            .send()
            .await
            .context("youtube music search")?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("youtube music search returned {status}"));
        }
        let v: Value = resp.json().await.context("youtube music search json")?;
        let mut items = Vec::new();
        collect_list_items(&v, &mut items);
        Ok(items
            .into_iter()
            .filter_map(|i| parse_album_item(i, params.is_none()))
            .collect())
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

/// `"Album - After Hours"` -> `"After Hours"`.
fn strip_album_prefix(title: &str) -> String {
    for p in ["Album - ", "Single - ", "EP - "] {
        if let Some(rest) = title.strip_prefix(p) {
            return rest.to_string();
        }
    }
    title.to_string()
}

/// Every `musicResponsiveListItemRenderer` anywhere in the response.
fn collect_list_items<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            if let Some(item) = m.get("musicResponsiveListItemRenderer") {
                out.push(item);
            }
            for x in m.values() {
                collect_list_items(x, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_list_items(x, out);
            }
        }
        _ => {}
    }
}

fn flex_runs(col: &Value) -> Vec<String> {
    col["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
        .as_array()
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r["text"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// First `playlistId` that looks like an album playlist, anywhere in the item.
fn find_album_playlist_id(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => {
            if let Some(id) = m.get("playlistId").and_then(Value::as_str)
                && is_album_playlist(id)
            {
                return Some(id.to_string());
            }
            m.values().find_map(find_album_playlist_id)
        }
        Value::Array(a) => a.iter().find_map(find_album_playlist_id),
        _ => None,
    }
}

const ALBUM_TYPES: &[&str] = &["Album", "Single", "EP"];

/// Subtitle runs look like `["Album", " • ", "The Weeknd", " & ", "Daft Punk", " • ", "2020"]`.
/// With the albums filter the type run can be missing. `require_type` is set for
/// unfiltered searches so songs, videos and playlists are skipped.
fn parse_album_item(item: &Value, require_type: bool) -> Option<Candidate> {
    let cols = item["flexColumns"].as_array()?;
    let title = flex_runs(cols.first()?).into_iter().next()?;
    let sub = cols.get(1).map(flex_runs).unwrap_or_default();
    let (kind, rest): (&str, &[String]) = match sub.first().map(String::as_str) {
        Some(k) if ALBUM_TYPES.contains(&k) => (k, &sub[1..]),
        _ if require_type => return None,
        _ => ("", &sub[..]),
    };
    let artists: Vec<String> = rest
        .iter()
        .skip_while(|s| s.trim() == "•")
        .take_while(|s| s.trim() != "•")
        .filter(|s| !matches!(s.trim(), "&" | "," | ""))
        .cloned()
        .collect();
    let id = find_album_playlist_id(item)?;
    Some(Candidate {
        music_ref: MusicRef {
            service: Service::YoutubeMusic,
            kind: Kind::Album,
            id,
        },
        title,
        artists,
        duration_ms: None,
        bonus: if kind == "Album" { 0.05 } else { 0.0 },
        genres: Vec::new(),
        duration_reliable: true,
    })
}

fn duration_of(v: &Video) -> Option<u64> {
    v.content_details
        .as_ref()
        .and_then(|c| c.duration.as_deref())
        .and_then(parse_iso8601_ms)
}

fn topics_of(t: Option<TopicDetails>) -> Vec<String> {
    t.map(|t| t.topic_categories).unwrap_or_default()
}

fn video_candidate(v: Video) -> Candidate {
    let (channel_artist, is_topic) = channel_to_artist(&v.snippet.channel_title);
    Candidate {
        duration_ms: duration_of(&v),
        music_ref: MusicRef {
            service: Service::YoutubeMusic,
            kind: Kind::Track,
            id: v.id,
        },
        title: v.snippet.title,
        artists: vec![channel_artist],
        bonus: if is_topic { 0.05 } else { 0.0 },
        genres: topics_of(v.topic_details),
        duration_reliable: is_topic,
    }
}

/// `PT3M20S` -> 200000. Handles the W/D/H/M/S designators YouTube emits.
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
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_iso8601_ms("PT3M20S"), Some(200_000));
        assert_eq!(parse_iso8601_ms("PT1H2M3S"), Some(3_723_000));
        assert_eq!(parse_iso8601_ms("PT45S"), Some(45_000));
        assert_eq!(parse_iso8601_ms("P1DT1S"), Some(86_401_000));
        assert_eq!(parse_iso8601_ms("PT0S"), Some(0));
        assert_eq!(parse_iso8601_ms("3:20"), None);
    }

    #[test]
    fn album_prefix() {
        assert_eq!(strip_album_prefix("Album - After Hours"), "After Hours");
        assert_eq!(
            strip_album_prefix("Single - Save Your Tears"),
            "Save Your Tears"
        );
        assert_eq!(strip_album_prefix("After Hours"), "After Hours");
    }

    #[test]
    fn parses_innertube_album_item() {
        let item = json!({
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "After Hours"}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                    {"text": "Album"}, {"text": " • "}, {"text": "The Weeknd"}, {"text": " & "}, {"text": "Someone"}, {"text": " • "}, {"text": "2020"}
                ]}}}
            ],
            "overlay": {"x": {"playNavigationEndpoint": {"watchPlaylistEndpoint": {"playlistId": "OLAK5uy_l4UqNJCpAF3kNaV37LHdRc_A07MmVdiSU"}}}},
            "navigationEndpoint": {"browseEndpoint": {"browseId": "MPREb_TH6Wut5eTMQ"}}
        });
        let c = parse_album_item(&item, true).expect("album");
        assert_eq!(c.title, "After Hours");
        assert_eq!(c.artists, vec!["The Weeknd", "Someone"]);
        assert_eq!(c.music_ref.id, "OLAK5uy_l4UqNJCpAF3kNaV37LHdRc_A07MmVdiSU");
        assert!(c.bonus > 0.0);

        // Song rows are skipped in unfiltered mode but a type-less row passes in filtered mode.
        let song = json!({
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Blinding Lights"}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Song"}, {"text": " • "}, {"text": "The Weeknd"}]}}}
            ],
            "playlistId": "OLAK5uy_l4UqNJCpAF3kNaV37LHdRc_A07MmVdiSU"
        });
        assert!(parse_album_item(&song, true).is_none());
        let typeless = json!({
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "After Hours"}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "The Weeknd"}, {"text": " • "}, {"text": "2020"}]}}}
            ],
            "playlistId": "OLAK5uy_l4UqNJCpAF3kNaV37LHdRc_A07MmVdiSU"
        });
        assert_eq!(
            parse_album_item(&typeless, false).unwrap().artists,
            vec!["The Weeknd"]
        );
        // No album playlist id -> skipped.
        assert!(parse_album_item(&json!({"flexColumns": [{"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "x"}]}}}]}), false).is_none());
    }

    #[test]
    fn collects_nested_items() {
        let v = json!({"contents": [{"musicShelfRenderer": {"contents": [
            {"musicResponsiveListItemRenderer": {"a": 1}},
            {"musicResponsiveListItemRenderer": {"a": 2}}
        ]}}]});
        let mut out = Vec::new();
        collect_list_items(&v, &mut out);
        assert_eq!(out.len(), 2);
    }
}
