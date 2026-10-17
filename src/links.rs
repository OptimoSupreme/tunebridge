//! Find Spotify / YouTube Music references inside free-form message text.

use crate::model::{Kind, MusicRef, Service};
use regex::Regex;
use std::sync::LazyLock;
use url::Url;

/// Something in a message that looks like music. `Ref` is fully identified;
/// the other variants need a network round-trip before they become a `MusicRef`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Found {
    Ref(MusicRef),
    /// A `spotify.link/...` short URL; expand by following redirects.
    SpotifyShort(String),
    /// A `music.youtube.com/browse/MPREb_...` album page; needs the page HTML
    /// to find the `OLAK5uy_` playlist id.
    YtmBrowse(String),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Also treat plain `youtube.com/watch` and `youtu.be` links as music.
    pub plain_youtube: bool,
}

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'`\)\]\}]+"#).expect("url regex"));
static SPOTIFY_URI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"spotify:(track|album):([A-Za-z0-9]{22})").expect("uri regex"));
static SPOTIFY_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/(?:intl-[A-Za-z-]+/)?(track|album)/([A-Za-z0-9]{22})").expect("path regex")
});
static YT_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").expect("yt id regex"));

/// Extract from a Matrix message: the plain `body` plus, if present, the
/// HTML `formatted_body` (which clients sometimes populate with the only
/// un-truncated copy of a link).
pub fn extract_from_message(body: &str, formatted: Option<&str>, opts: Options) -> Vec<Found> {
    let mut out = extract(body, opts);
    if let Some(html) = formatted {
        for f in extract(&unescape_html(html), opts) {
            push_unique(&mut out, f);
        }
    }
    out
}

pub fn extract(text: &str, opts: Options) -> Vec<Found> {
    let mut out = Vec::new();
    for m in URL_RE.find_iter(text) {
        if let Some(f) = classify_url(trim_trailing_punct(m.as_str()), opts) {
            push_unique(&mut out, f);
        }
    }
    for c in SPOTIFY_URI_RE.captures_iter(text) {
        let kind = if &c[1] == "track" {
            Kind::Track
        } else {
            Kind::Album
        };
        push_unique(
            &mut out,
            Found::Ref(MusicRef {
                service: Service::Spotify,
                kind,
                id: c[2].to_string(),
            }),
        );
    }
    out
}

/// Classify one absolute URL. Public so providers can reuse it on the final
/// URL after following a short-link redirect.
pub fn classify_url(raw: &str, opts: Options) -> Option<Found> {
    let url = Url::parse(raw).ok()?;
    let host_owned = url.host_str()?.to_ascii_lowercase();
    let host = host_owned.strip_prefix("www.").unwrap_or(&host_owned);
    match host {
        "open.spotify.com" => {
            let caps = SPOTIFY_PATH_RE.captures(url.path())?;
            let kind = if &caps[1] == "track" {
                Kind::Track
            } else {
                Kind::Album
            };
            Some(Found::Ref(MusicRef {
                service: Service::Spotify,
                kind,
                id: caps[2].to_string(),
            }))
        }
        "spotify.link" => Some(Found::SpotifyShort(raw.to_string())),
        "music.youtube.com" => classify_youtube(&url, true),
        "youtube.com" | "m.youtube.com" if opts.plain_youtube => classify_youtube(&url, false),
        "youtu.be" if opts.plain_youtube => {
            let id = url.path().trim_start_matches('/');
            YT_ID_RE.is_match(id).then_some(Found::Ref(MusicRef {
                service: Service::YoutubeMusic,
                kind: Kind::Track,
                id: id.to_string(),
            }))
        }
        _ => None,
    }
}

fn classify_youtube(url: &Url, music_host: bool) -> Option<Found> {
    let param = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    match url.path() {
        "/watch" => {
            let id = param("v")?;
            YT_ID_RE.is_match(&id).then_some(Found::Ref(MusicRef {
                service: Service::YoutubeMusic,
                kind: Kind::Track,
                id,
            }))
        }
        "/playlist" => {
            let list = param("list")?;
            // Auto-generated album playlists carry this prefix; anything else is a
            // user playlist, which we do not translate.
            list.starts_with("OLAK5uy_").then_some(Found::Ref(MusicRef {
                service: Service::YoutubeMusic,
                kind: Kind::Album,
                id: list,
            }))
        }
        p if music_host && p.starts_with("/browse/MPREb_") => {
            Some(Found::YtmBrowse(p["/browse/".len()..].to_string()))
        }
        _ => None,
    }
}

fn trim_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(['.', ',', ';', ':', '!', '?'])
}

fn push_unique(out: &mut Vec<Found>, f: Found) {
    if !out.contains(&f) {
        out.push(f);
    }
}

pub fn unescape_html(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(kind: Kind, id: &str) -> Found {
        Found::Ref(MusicRef {
            service: Service::Spotify,
            kind,
            id: id.into(),
        })
    }
    fn yt(kind: Kind, id: &str) -> Found {
        Found::Ref(MusicRef {
            service: Service::YoutubeMusic,
            kind,
            id: id.into(),
        })
    }
    const T: &str = "0VjIjW4GlUZAMYd2vXMi3b";
    const A: &str = "4yP0hdKOZPNshxUOjY0cZj";

    #[test]
    fn spotify_track_with_si_and_trailing_punct() {
        let text = format!("check this out https://open.spotify.com/track/{T}?si=abc123!");
        assert_eq!(extract(&text, Options::default()), vec![sp(Kind::Track, T)]);
    }

    #[test]
    fn spotify_intl_prefix_and_album() {
        let text = format!("https://open.spotify.com/intl-de/album/{A}");
        assert_eq!(extract(&text, Options::default()), vec![sp(Kind::Album, A)]);
    }

    #[test]
    fn spotify_uri_and_short_link() {
        let text = format!("spotify:track:{T} and https://spotify.link/AbCdEf");
        assert_eq!(
            extract(&text, Options::default()),
            vec![
                Found::SpotifyShort("https://spotify.link/AbCdEf".into()),
                sp(Kind::Track, T)
            ]
        );
    }

    #[test]
    fn ytm_watch_playlist_and_browse() {
        let text = "https://music.youtube.com/watch?v=4NRXx6U8ABQ&list=RDAMVM4NRXx6U8ABQ \
                    https://music.youtube.com/playlist?list=OLAK5uy_kX8s4ZQ1 \
                    https://music.youtube.com/browse/MPREb_abc123 \
                    https://music.youtube.com/playlist?list=PLuserplaylist";
        assert_eq!(
            extract(text, Options::default()),
            vec![
                yt(Kind::Track, "4NRXx6U8ABQ"),
                yt(Kind::Album, "OLAK5uy_kX8s4ZQ1"),
                Found::YtmBrowse("MPREb_abc123".into()),
            ]
        );
    }

    #[test]
    fn plain_youtube_only_when_enabled() {
        let text = "https://www.youtube.com/watch?v=4NRXx6U8ABQ https://youtu.be/dQw4w9WgXcQ";
        assert!(extract(text, Options::default()).is_empty());
        assert_eq!(
            extract(
                text,
                Options {
                    plain_youtube: true
                }
            ),
            vec![
                yt(Kind::Track, "4NRXx6U8ABQ"),
                yt(Kind::Track, "dQw4w9WgXcQ")
            ]
        );
    }

    #[test]
    fn formatted_body_with_escaped_ampersand_dedupes() {
        let body = "https://music.youtube.com/watch?v=4NRXx6U8ABQ&feature=share";
        let html =
            r#"<a href="https://music.youtube.com/watch?v=4NRXx6U8ABQ&amp;feature=share">link</a>"#;
        assert_eq!(
            extract_from_message(body, Some(html), Options::default()),
            vec![yt(Kind::Track, "4NRXx6U8ABQ")]
        );
    }

    #[test]
    fn ignores_unrelated_urls() {
        assert!(
            extract(
                "https://example.com/track/abc and https://open.spotify.com/user/x",
                Options::default()
            )
            .is_empty()
        );
    }
}
