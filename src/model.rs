//! Core data types shared by link parsing, providers, matching and replies.

use serde::{Deserialize, Serialize};

/// A music streaming service we know how to read links from and search on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    Spotify,
    YoutubeMusic,
}

impl Service {
    pub fn display_name(self) -> &'static str {
        match self {
            Service::Spotify => "Spotify",
            Service::YoutubeMusic => "YouTube Music",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Track,
    Album,
}

/// A fully identified item on one service: enough to build a canonical URL
/// and to ask that service's API for metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MusicRef {
    pub service: Service,
    pub kind: Kind,
    pub id: String,
}

impl MusicRef {
    pub fn url(&self) -> String {
        match (self.service, self.kind) {
            (Service::Spotify, Kind::Track) => {
                format!("https://open.spotify.com/track/{}", self.id)
            }
            (Service::Spotify, Kind::Album) => {
                format!("https://open.spotify.com/album/{}", self.id)
            }
            (Service::YoutubeMusic, Kind::Track) => {
                format!("https://music.youtube.com/watch?v={}", self.id)
            }
            (Service::YoutubeMusic, Kind::Album) => {
                format!("https://music.youtube.com/playlist?list={}", self.id)
            }
        }
    }
}

/// Metadata for a track or album as resolved from its home service.
#[derive(Debug, Clone)]
pub struct Item {
    pub source: MusicRef,
    pub title: String,
    pub artists: Vec<String>,
    pub duration_ms: Option<u64>,
    pub isrc: Option<String>,
}

impl Item {
    pub fn kind(&self) -> Kind {
        self.source.kind
    }

    pub fn primary_artist(&self) -> &str {
        self.artists.first().map(String::as_str).unwrap_or("")
    }

    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }
}

/// A search hit on a target service, before scoring.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub music_ref: MusicRef,
    pub title: String,
    pub artists: Vec<String>,
    pub duration_ms: Option<u64>,
    /// Small additive nudge for hints like "- Topic" channels or album playlists.
    pub bonus: f64,
}

#[derive(Debug, Clone)]
pub struct Translation {
    pub service: Service,
    pub url: String,
}

/// One resolved source item plus every target link we found for it.
#[derive(Debug, Clone)]
pub struct Reply {
    pub item: Item,
    pub translations: Vec<Translation>,
}
