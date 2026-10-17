//! Fuzzy matching between an item resolved on one service and search hits on
//! another. Everything here is pure and unit-tested; tune the weights with the
//! table in the tests.

use crate::model::{Candidate, Item};
use regex::Regex;
use std::sync::LazyLock;
use strsim::jaro_winkler;

/// Minimum score for a candidate to be accepted as "the same recording".
pub const THRESHOLD: f64 = 0.62;

const W_TITLE: f64 = 0.5;
const W_ARTIST: f64 = 0.3;
const W_DURATION: f64 = 0.2;

/// Bracketed or dash-suffixed fragments that carry no identity: `(Official Video)`,
/// `[HD]`, `- Remastered 2011`, `(feat. X)`. Removed before comparing titles.
static NOISE_WORDS: &[&str] = &[
    "feat",
    "ft.",
    "ft ",
    "featuring",
    "official",
    "video",
    "audio",
    "lyric",
    "lyrics",
    "visualizer",
    "visualiser",
    "hd",
    "hq",
    "4k",
    "remaster",
    "remastered",
    "explicit",
    "clean",
    "mono",
    "stereo",
    "album version",
    "single version",
    "original mix",
    "bonus track",
    "with lyrics",
    "music video",
    "deluxe",
    "edition",
    "anniversary",
    "expanded",
    "reissue",
];

/// Words that change *which* recording this is. A mismatch is penalised.
static VARIANT_WORDS: &[&str] = &[
    "live",
    "remix",
    "acoustic",
    "instrumental",
    "karaoke",
    "cover",
    "sped up",
    "slowed",
    "nightcore",
    "reverb",
    "demo",
    "unplugged",
    "orchestral",
    "piano version",
    "8d",
];

static BRACKET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\(\[\{][^\)\]\}]*[\)\]\}]").expect("bracket regex"));
static DASH_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+[-–—|]\s+[^-–—|]*$").expect("dash regex"));
static FEAT_TAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(feat\.?|ft\.?|featuring)\s.*$").expect("feat regex"));
static NON_ALNUM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").expect("alnum regex"));

fn has_noise(fragment: &str) -> bool {
    let f = fragment.to_lowercase();
    NOISE_WORDS.iter().any(|w| f.contains(w))
}

/// Strip noise brackets / suffixes but keep casing and punctuation. Used to
/// build search queries that still read naturally.
pub fn clean_for_query(title: &str) -> String {
    let mut s = BRACKET_RE
        .replace_all(title, |c: &regex::Captures| {
            if has_noise(&c[0]) {
                String::new()
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    while let Some(m) = DASH_SUFFIX_RE.find(&s) {
        if !has_noise(m.as_str()) {
            break;
        }
        s.truncate(m.start());
    }
    let s = FEAT_TAIL_RE.replace(&s, "").into_owned();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercase, de-noised, alphanumeric-only form used for similarity.
pub fn normalize(s: &str) -> String {
    let cleaned = clean_for_query(s).to_lowercase();
    let cleaned = cleaned.replace('&', " and ");
    NON_ALNUM_RE.replace_all(&cleaned, " ").trim().to_string()
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Title variants worth comparing: the whole thing, and for `A - B` shaped
/// titles (common on YouTube) each half on its own.
fn title_variants(title: &str) -> Vec<String> {
    let mut v = vec![normalize(title)];
    for sep in [" - ", " – ", " — ", " | "] {
        if let Some((a, b)) = title.split_once(sep) {
            v.push(normalize(a));
            v.push(normalize(b));
        }
    }
    v.retain(|s| !s.is_empty());
    v
}

fn title_similarity(a: &str, b: &str) -> f64 {
    let va = title_variants(a);
    let vb = title_variants(b);
    va.iter()
        .flat_map(|x| vb.iter().map(move |y| jaro_winkler(x, y)))
        .fold(0.0, f64::max)
}

fn artist_score(item: &Item, cand: &Candidate) -> f64 {
    let cand_blob = normalize(&format!("{} {}", cand.artists.join(" "), cand.title));
    let cand_squashed = squash(&cand_blob);
    let item_blob = normalize(&format!("{} {}", item.artists.join(" "), item.title));
    let item_squashed = squash(&item_blob);

    let contains = |blob: &str, squashed: &str, name: &str| {
        let n = normalize(name);
        !n.is_empty() && (blob.contains(&n) || squashed.contains(&squash(&n)))
    };

    let mut best: f64 = 0.0;
    for (i, a) in item.artists.iter().enumerate() {
        if contains(&cand_blob, &cand_squashed, a) {
            best = best.max(if i == 0 { 1.0 } else { 0.8 });
        }
    }
    for a in &cand.artists {
        if contains(&item_blob, &item_squashed, a) {
            best = best.max(0.9);
        }
    }
    if best == 0.0
        && let (Some(a), Some(b)) = (item.artists.first(), cand.artists.first())
    {
        let jw = jaro_winkler(&squash(&normalize(a)), &squash(&normalize(b)));
        if jw >= 0.85 {
            best = jw;
        }
    }
    best
}

fn duration_score(a: u64, b: u64) -> f64 {
    let diff = (a as f64 - b as f64).abs() / 1000.0;
    if diff <= 3.0 {
        1.0
    } else if diff <= 15.0 {
        1.0 - (diff - 3.0) / 12.0 * 0.7
    } else {
        0.0
    }
}

fn variant_penalty(a: &str, b: &str) -> f64 {
    let la = a.to_lowercase();
    let lb = b.to_lowercase();
    let mismatches = VARIANT_WORDS
        .iter()
        .filter(|w| la.contains(*w) != lb.contains(*w))
        .count();
    (mismatches as f64 * 0.3).min(0.45)
}

/// Score in `[0, 1]`; anything below [`THRESHOLD`] should be discarded.
pub fn score(item: &Item, cand: &Candidate) -> f64 {
    // Jaro-Winkler sits around 0.5-0.7 even for unrelated short strings, so
    // gate hard and rescale the useful 0.75..1.0 band onto 0..1.
    let jw = title_similarity(&item.title, &cand.title);
    if jw < 0.75 {
        return 0.0;
    }
    let title = ((jw - 0.75) / 0.25).clamp(0.0, 1.0);
    let artist = artist_score(item, cand);

    let mut total = title * W_TITLE + artist * W_ARTIST;
    let mut weight = W_TITLE + W_ARTIST;
    if let (Some(a), Some(b)) = (item.duration_ms, cand.duration_ms) {
        let d = duration_score(a, b);
        if d == 0.0 {
            // Same song never differs by more than 15 s between services.
            return 0.0;
        }
        total += d * W_DURATION;
        weight += W_DURATION;
    }
    let s = total / weight - variant_penalty(&item.title, &cand.title) + cand.bonus;
    s.clamp(0.0, 1.0)
}

/// Pick the best candidate at or above [`THRESHOLD`].
pub fn best<'a>(item: &Item, cands: &'a [Candidate]) -> Option<&'a Candidate> {
    let mut scored: Vec<(f64, &Candidate)> = cands.iter().map(|c| (score(item, c), c)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (s, c) in &scored {
        tracing::debug!(score = format!("{s:.3}"), title = %c.title, artists = ?c.artists, url = %c.music_ref.url(), "candidate");
    }
    scored
        .first()
        .filter(|(s, _)| *s >= THRESHOLD)
        .map(|(_, c)| *c)
}

/// YouTube gives us a video title and a channel; turn that into the best
/// guess at `(artist, title)`. `channel_artist` is the channel name with
/// `- Topic` / `VEVO` already stripped.
pub fn split_artist_title(
    video_title: &str,
    channel_artist: &str,
    is_topic: bool,
) -> (String, String) {
    let cleaned = clean_for_query(video_title);
    for sep in [" - ", " – ", " — ", " | ", ": "] {
        if let Some((left, right)) = cleaned.split_once(sep) {
            let left_n = squash(&normalize(left));
            let chan_n = squash(&normalize(channel_artist));
            let looks_like_artist = is_topic
                || chan_n.is_empty()
                || left_n == chan_n
                || jaro_winkler(&left_n, &chan_n) >= 0.8
                || chan_n.contains(&left_n)
                || left_n.contains(&chan_n);
            if looks_like_artist && !right.trim().is_empty() {
                return (left.trim().to_string(), right.trim().to_string());
            }
        }
    }
    (channel_artist.trim().to_string(), cleaned)
}

/// `"The Weeknd - Topic"` -> `("The Weeknd", true)`, `"TheWeekndVEVO"` -> `("TheWeeknd", false)`.
pub fn channel_to_artist(channel: &str) -> (String, bool) {
    if let Some(a) = channel.strip_suffix(" - Topic") {
        return (a.trim().to_string(), true);
    }
    let trimmed = channel.trim();
    let lower = trimmed.to_lowercase();
    if lower.ends_with("vevo") {
        return (trimmed[..trimmed.len() - 4].trim().to_string(), false);
    }
    if let Some(a) = lower.strip_suffix("official") {
        return (trimmed[..a.len()].trim().to_string(), false);
    }
    (trimmed.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, MusicRef, Service};

    fn item(title: &str, artists: &[&str], dur: Option<u64>) -> Item {
        Item {
            source: MusicRef {
                service: Service::Spotify,
                kind: if dur.is_some() {
                    Kind::Track
                } else {
                    Kind::Album
                },
                id: "x".into(),
            },
            title: title.into(),
            artists: artists.iter().map(|s| s.to_string()).collect(),
            duration_ms: dur,
            isrc: None,
        }
    }
    fn cand(title: &str, artists: &[&str], dur: Option<u64>, bonus: f64) -> Candidate {
        Candidate {
            music_ref: MusicRef {
                service: Service::YoutubeMusic,
                kind: Kind::Track,
                id: title.into(),
            },
            title: title.into(),
            artists: artists.iter().map(|s| s.to_string()).collect(),
            duration_ms: dur,
            bonus,
        }
    }

    #[test]
    fn normalize_strips_noise() {
        assert_eq!(
            normalize("Blinding Lights (Official Video) [HD]"),
            "blinding lights"
        );
        assert_eq!(
            normalize("Hotel California - 2013 Remaster"),
            "hotel california"
        );
        assert_eq!(
            normalize("Peaches (feat. Daniel Caesar & Giveon)"),
            "peaches"
        );
        assert_eq!(normalize("Peaches ft. Daniel Caesar"), "peaches");
        assert_eq!(
            normalize("Love Story (Taylor's Version)"),
            "love story taylor s version"
        );
        assert_eq!(normalize("Rock & Roll"), "rock and roll");
    }

    #[test]
    fn picks_topic_upload_over_live_version() {
        let it = item("Blinding Lights", &["The Weeknd"], Some(200_040));
        let cands = vec![
            cand(
                "Blinding Lights (Live)",
                &["The Weeknd"],
                Some(230_000),
                0.0,
            ),
            cand(
                "The Weeknd - Blinding Lights (Official Video)",
                &["TheWeekndVEVO"],
                Some(262_000),
                0.0,
            ),
            cand("Blinding Lights", &["The Weeknd"], Some(201_000), 0.05),
            cand("Save Your Tears", &["The Weeknd"], Some(215_000), 0.05),
        ];
        let b = best(&it, &cands).expect("match");
        assert_eq!(b.title, "Blinding Lights");
    }

    #[test]
    fn artist_dash_title_from_youtube_matches_spotify() {
        let it = item("Everlong", &["Foo Fighters"], Some(250_000));
        let cands = [cand(
            "Foo Fighters - Everlong",
            &["Foo Fighters"],
            Some(251_000),
            0.0,
        )];
        assert!(score(&it, &cands[0]) > 0.9);
    }

    #[test]
    fn rejects_wrong_song_and_wrong_duration() {
        let it = item("Everlong", &["Foo Fighters"], Some(250_000));
        assert_eq!(
            score(&it, &cand("My Hero", &["Foo Fighters"], Some(250_000), 0.0)),
            0.0
        );
        assert_eq!(
            score(
                &it,
                &cand("Everlong", &["Foo Fighters"], Some(300_000), 0.0)
            ),
            0.0
        );
        assert!(
            best(
                &it,
                &[cand(
                    "Everlong (Acoustic)",
                    &["Foo Fighters"],
                    Some(252_000),
                    0.0
                )]
            )
            .is_none()
        );
    }

    #[test]
    fn album_without_duration_still_matches() {
        let it = item("The Colour and the Shape", &["Foo Fighters"], None);
        let cands = vec![
            cand(
                "The Colour and the Shape",
                &["Foo Fighters - Topic"],
                None,
                0.05,
            ),
            cand("Foo Fighters Greatest Hits", &["Foo Fighters"], None, 0.0),
        ];
        assert_eq!(best(&it, &cands).unwrap().title, "The Colour and the Shape");
    }

    #[test]
    fn vevo_channel_artist_still_counts() {
        let it = item(
            "Peaches",
            &["Justin Bieber", "Daniel Caesar", "Giveon"],
            Some(198_000),
        );
        let c = cand(
            "Justin Bieber - Peaches ft. Daniel Caesar, Giveon",
            &["JustinBieber"],
            Some(199_000),
            0.0,
        );
        assert!(score(&it, &c) > 0.85, "{}", score(&it, &c));
    }

    #[test]
    fn split_artist_title_cases() {
        assert_eq!(
            split_artist_title(
                "Foo Fighters - Everlong (Official HD Video)",
                "Foo Fighters",
                false
            ),
            ("Foo Fighters".into(), "Everlong".into())
        );
        assert_eq!(
            split_artist_title("Everlong", "Foo Fighters", true),
            ("Foo Fighters".into(), "Everlong".into())
        );
        assert_eq!(
            split_artist_title("Everlong", "Some Random Channel", false),
            ("Some Random Channel".into(), "Everlong".into())
        );
        assert_eq!(
            split_artist_title("The Weeknd - Blinding Lights", "TheWeeknd", false),
            ("The Weeknd".into(), "Blinding Lights".into())
        );
    }

    #[test]
    fn channel_to_artist_cases() {
        assert_eq!(
            channel_to_artist("The Weeknd - Topic"),
            ("The Weeknd".into(), true)
        );
        assert_eq!(
            channel_to_artist("TheWeekndVEVO"),
            ("TheWeeknd".into(), false)
        );
        assert_eq!(
            channel_to_artist("Foo Fighters"),
            ("Foo Fighters".into(), false)
        );
    }
}
