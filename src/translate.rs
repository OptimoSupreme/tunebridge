//! Orchestration: message text -> found links -> resolved item -> links on
//! every other configured service -> formatted reply.

use crate::config::Config;
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
        Ok(Some(Reply { item, translations }))
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
        if r.translations.is_empty() {
            t.push_str("\nNo match found");
            h.push_str("<br>No match found");
        }
        for tr in &r.translations {
            t.push_str(&format!("\n{}: {}", tr.service.display_name(), tr.url));
            h.push_str(&format!(
                "<br>{}: <a href=\"{u}\">{u}</a>",
                tr.service.display_name(),
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

    #[test]
    fn formats_track_reply() {
        let reply = Reply {
            item: Item {
                source: MusicRef {
                    service: Service::Spotify,
                    kind: Kind::Track,
                    id: "x".into(),
                },
                title: "Rock & Roll".into(),
                artists: vec!["Led Zeppelin".into()],
                duration_ms: Some(1),
                isrc: None,
            },
            translations: vec![Translation {
                service: Service::YoutubeMusic,
                url: "https://music.youtube.com/watch?v=abc".into(),
            }],
        };
        let (t, h) = format_replies(&[reply], false).unwrap();
        assert_eq!(
            t,
            "\u{1F3B5} Rock & Roll – Led Zeppelin\nYouTube Music: https://music.youtube.com/watch?v=abc"
        );
        assert!(h.contains("<b>Rock &amp; Roll</b>"));
        assert!(h.contains("<a href=\"https://music.youtube.com/watch?v=abc\">"));
    }

    #[test]
    fn silent_on_miss_by_default() {
        let reply = Reply {
            item: Item {
                source: MusicRef {
                    service: Service::Spotify,
                    kind: Kind::Album,
                    id: "x".into(),
                },
                title: "A".into(),
                artists: vec![],
                duration_ms: None,
                isrc: None,
            },
            translations: vec![],
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
