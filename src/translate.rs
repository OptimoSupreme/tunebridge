//! Orchestration: message text -> found links -> resolved item -> links on
//! every other configured service -> formatted reply.

use crate::config::Config;
use crate::genre::{self, Genre};
use crate::links::{self, Found, Options};
use crate::matching;
use crate::model::{Item, Kind, MusicRef, Reply, Service, Translation};
use crate::providers::{USER_AGENT, spotify::Spotify, youtube::YouTube};
use anyhow::{Context, Result};
use moka::future::Cache;
use std::sync::Arc;
use std::time::Duration;

pub struct Translator {
    spotify: Option<Spotify>,
    youtube: Option<YouTube>,
    targets: Vec<Service>,
    link_opts: Options,
    max_links: usize,
    genre_emojis: bool,
    cache: Cache<Found, Arc<Reply>>,
}

impl Translator {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()
            .context("building http client")?;
        Ok(Self {
            spotify: cfg.spotify.as_ref().map(|c| Spotify::new(http.clone(), c)),
            youtube: cfg.youtube.as_ref().map(|c| YouTube::new(http.clone(), c)),
            targets: cfg.behaviour.targets.clone(),
            link_opts: Options {
                plain_youtube: cfg.behaviour.translate_plain_youtube,
            },
            max_links: cfg.behaviour.max_links_per_message,
            genre_emojis: cfg.behaviour.genre_emojis,
            cache: Cache::builder()
                .max_capacity(2_000)
                .time_to_live(Duration::from_secs(24 * 3600))
                .build(),
        })
    }

    /// Everything we could translate in one message, in order of appearance.
    pub async fn translate_message(&self, body: &str, formatted: Option<&str>) -> Vec<Reply> {
        let found = links::extract_from_message(body, formatted, self.link_opts);
        let mut replies = Vec::new();
        for f in found.into_iter().take(self.max_links) {
            match self.translate_cached(f.clone()).await {
                Ok(Some(r)) => replies.push(r),
                Ok(None) => tracing::info!(?f, "could not resolve link to an item"),
                Err(e) => tracing::warn!(?f, error = %e, "translation failed"),
            }
        }
        replies
    }

    async fn translate_cached(&self, f: Found) -> Result<Option<Reply>> {
        if let Some(hit) = self.cache.get(&f).await {
            tracing::debug!(?f, "cache hit");
            return Ok(Some((*hit).clone()));
        }
        let reply = self.translate_one(&f).await?;
        // Only cache complete successes so transient API failures get retried.
        if let Some(r) = &reply
            && !r.translations.is_empty()
        {
            self.cache.insert(f, Arc::new(r.clone())).await;
        }
        Ok(reply)
    }

    async fn to_ref(&self, f: &Found) -> Result<Option<MusicRef>> {
        Ok(match f {
            Found::Ref(r) => Some(r.clone()),
            Found::SpotifyShort(url) => match &self.spotify {
                Some(s) => s.expand_short_link(url).await?,
                None => None,
            },
            Found::YtmBrowse(id) => match &self.youtube {
                Some(y) => y.browse_to_playlist(id).await?.map(|list| MusicRef {
                    service: Service::YoutubeMusic,
                    kind: Kind::Album,
                    id: list,
                }),
                None => None,
            },
        })
    }

    async fn resolve(&self, r: &MusicRef) -> Result<Item> {
        match r.service {
            Service::Spotify => {
                self.spotify
                    .as_ref()
                    .context("spotify not configured")?
                    .resolve(r)
                    .await
            }
            Service::YoutubeMusic => {
                self.youtube
                    .as_ref()
                    .context("youtube not configured")?
                    .resolve(r)
                    .await
            }
        }
    }

    async fn translate_one(&self, f: &Found) -> Result<Option<Reply>> {
        let Some(source) = self.to_ref(f).await? else {
            return Ok(None);
        };
        let item = self.resolve(&source).await?;
        tracing::info!(
            source = %source.url(),
            title = %item.title,
            artists = ?item.artists,
            duration_ms = ?item.duration_ms,
            "resolved"
        );

        let mut translations = Vec::new();
        let mut extra_tags: Vec<String> = Vec::new();
        for &target in &self.targets {
            if target == source.service {
                continue;
            }
            let cands = match target {
                Service::Spotify => match &self.spotify {
                    Some(s) => s.search(&item).await,
                    None => continue,
                },
                Service::YoutubeMusic => match &self.youtube {
                    Some(y) => y.search(&item).await,
                    None => continue,
                },
            };
            match cands {
                Ok(cands) => match matching::best(&item, &cands) {
                    Some(c) => {
                        tracing::info!(target = target.display_name(), url = %c.music_ref.url(), "matched");
                        translations.push(Translation {
                            service: target,
                            url: c.music_ref.url(),
                        });
                        extra_tags.extend(c.genres.iter().cloned());
                        if self.genre_emojis
                            && target == Service::Spotify
                            && !c.artist_ids.is_empty()
                            && let Some(s) = &self.spotify
                        {
                            match s.artist_genres(&c.artist_ids).await {
                                Ok(g) => extra_tags.extend(g),
                                Err(e) => {
                                    tracing::debug!(error = %e, "spotify artist genres unavailable")
                                }
                            }
                        }
                    }
                    None => tracing::info!(
                        target = target.display_name(),
                        candidates = cands.len(),
                        "no acceptable match"
                    ),
                },
                Err(e) => {
                    tracing::warn!(target = target.display_name(), error = %e, "search failed")
                }
            }
        }
        let genres = if self.genre_emojis {
            genre::for_item(&item, &extra_tags)
        } else {
            Vec::new()
        };
        if !genres.is_empty() {
            tracing::debug!(genres = %genre_names(&genres), "classified genre");
        }
        Ok(Some(Reply {
            item,
            translations,
            genres,
        }))
    }
}

/// Render replies as `(plain_text, html)`. Returns `None` when nothing is worth
/// posting (every item missed and `reply_on_miss` is off).
pub fn format_replies(replies: &[Reply], reply_on_miss: bool) -> Option<(String, String)> {
    let mut text = Vec::new();
    let mut html = Vec::new();
    for r in replies {
        if r.translations.is_empty() && !reply_on_miss {
            continue;
        }
        let icon = match r.item.kind() {
            Kind::Track => "\u{1F3B5}",
            Kind::Album => "\u{1F4BF}",
        };
        let artist = r.item.artist_line();
        let mut t = if artist.is_empty() {
            format!("{icon} {}", r.item.title)
        } else {
            format!("{icon} {} – {}", r.item.title, artist)
        };
        let mut h = if artist.is_empty() {
            format!("{icon} <b>{}</b>", esc(&r.item.title))
        } else {
            format!("{icon} <b>{}</b> – {}", esc(&r.item.title), esc(&artist))
        };
        let vibe = genre::emojis(&r.genres, &r.item.source.id);
        if !vibe.is_empty() {
            t.push(' ');
            t.push_str(&vibe);
            h.push(' ');
            h.push_str(&vibe);
        }
        if r.translations.is_empty() {
            t.push_str("\nNo match found");
            h.push_str("<br>No match found");
        }
        for tr in &r.translations {
            // Plain text: coloured square + name. HTML: the same square plus the
            // service name in its brand colour; `color` is for older clients,
            // `data-mx-color` is the spec attribute.
            t.push_str(&format!(
                "\n{} {}: {}",
                tr.service.badge(),
                tr.service.display_name(),
                tr.url
            ));
            h.push_str(&format!(
                "<br>{badge} <font color=\"{c}\" data-mx-color=\"{c}\"><b>{name}</b></font>: <a href=\"{u}\">{u}</a>",
                badge = tr.service.badge(),
                c = tr.service.color(),
                name = tr.service.display_name(),
                u = esc(&tr.url)
            ));
        }
        text.push(t);
        html.push(h);
    }
    if text.is_empty() {
        None
    } else {
        Some((text.join("\n\n"), html.join("<br><br>")))
    }
}

/// Human-readable genre list for logs.
pub fn genre_names(genres: &[Genre]) -> String {
    genres
        .iter()
        .map(|g| g.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MusicRef;

    fn item(kind: Kind, title: &str, artists: &[&str]) -> Item {
        Item {
            source: MusicRef {
                service: Service::Spotify,
                kind,
                id: "x".into(),
            },
            title: title.into(),
            artists: artists.iter().map(|s| s.to_string()).collect(),
            duration_ms: None,
            isrc: None,
            genres: Vec::new(),
        }
    }

    #[test]
    fn formats_track_reply_with_badge_and_genre() {
        let reply = Reply {
            item: item(Kind::Track, "Rock & Roll", &["Led Zeppelin"]),
            translations: vec![Translation {
                service: Service::YoutubeMusic,
                url: "https://music.youtube.com/watch?v=abc".into(),
            }],
            genres: vec![Genre::Rock],
        };
        let (t, h) = format_replies(&[reply], false).unwrap();
        let vibe = genre::emojis(&[Genre::Rock], "x");
        assert_eq!(
            t,
            format!(
                "\u{1F3B5} Rock & Roll – Led Zeppelin {vibe}\n\u{1F534} YouTube Music: https://music.youtube.com/watch?v=abc"
            )
        );
        assert!(h.contains("<b>Rock &amp; Roll</b>"));
        assert!(h.contains(&vibe));
        assert!(h.contains(
            "\u{1F534} <font color=\"#FF0000\" data-mx-color=\"#FF0000\"><b>YouTube Music</b></font>: "
        ));
        assert!(h.contains(
            "<a href=\"https://music.youtube.com/watch?v=abc\">https://music.youtube.com/watch?v=abc</a>"
        ));
    }

    #[test]
    fn no_genre_means_no_trailing_space() {
        let reply = Reply {
            item: item(Kind::Track, "Song", &["Band"]),
            translations: vec![Translation {
                service: Service::Spotify,
                url: "https://open.spotify.com/track/y".into(),
            }],
            genres: vec![],
        };
        let (t, _) = format_replies(&[reply], false).unwrap();
        assert_eq!(
            t,
            "\u{1F3B5} Song – Band\n\u{1F7E2} Spotify: https://open.spotify.com/track/y"
        );
        assert_eq!(genre_names(&[Genre::Rock, Genre::Pop]), "Rock, Pop");
    }

    #[test]
    fn silent_on_miss_by_default() {
        let reply = Reply {
            item: item(Kind::Album, "A", &[]),
            translations: vec![],
            genres: vec![],
        };
        assert!(format_replies(std::slice::from_ref(&reply), false).is_none());
        assert!(
            format_replies(&[reply], true)
                .unwrap()
                .0
                .contains("No match found")
        );
    }
}
