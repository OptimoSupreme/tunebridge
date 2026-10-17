//! Coarse genre classification from free-text tags (Spotify artist genres,
//! YouTube topic categories) and a few emoji sets per genre so replies don't
//! look identical every time.

use crate::model::Item;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Genre {
    Metal,
    Punk,
    Rock,
    HipHop,
    Electronic,
    Country,
    Folk,
    Jazz,
    Blues,
    Classical,
    RnbSoul,
    Reggae,
    Latin,
    Kpop,
    Jpop,
    Indie,
    Christian,
    Soundtrack,
    Children,
    Holiday,
    Pop,
}

impl fmt::Display for Genre {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Substring rules, checked in order. More specific genres come first so that
/// "pop punk" is punk, "k-pop" is K-pop, "indie rock" is indie before rock and
/// "rhythm and blues" is R&B before blues.
const RULES: &[(Genre, &[&str])] = &[
    (Genre::Holiday, &["christmas", "holiday", "xmas", "navidad"]),
    (Genre::Children, &["children", "kids", "nursery", "lullab"]),
    (
        Genre::Soundtrack,
        &[
            "soundtrack",
            "film score",
            "video game music",
            "anime",
            "musical",
            "broadway",
            "show tunes",
        ],
    ),
    (
        Genre::Christian,
        &["christian", "gospel", "worship", "ccm", "praise"],
    ),
    (Genre::Kpop, &["k-pop", "kpop", "korean"]),
    (
        Genre::Jpop,
        &[
            "j-pop", "jpop", "j-rock", "japanese", "vocaloid", "city pop",
        ],
    ),
    (
        Genre::Metal,
        &[
            "metal",
            "metalcore",
            "deathcore",
            "grindcore",
            "djent",
            "doom",
            "sludge",
        ],
    ),
    (
        Genre::Punk,
        &["punk", "hardcore", "emo", "ska punk", "screamo"],
    ),
    (
        Genre::Reggae,
        &["reggae", "dancehall", "dub", "ska", "roots"],
    ),
    (
        Genre::Latin,
        &[
            "latin",
            "reggaeton",
            "salsa",
            "bachata",
            "cumbia",
            "mariachi",
            "corrido",
            "banda",
            "merengue",
            "bossa",
            "samba",
            "tango",
            "flamenco",
            "urbano",
            "música mexicana",
            "musica mexicana",
            "regional mexican",
            "sertanejo",
            "funk carioca",
        ],
    ),
    (
        Genre::Country,
        &[
            "country",
            "americana",
            "bluegrass",
            "honky",
            "outlaw",
            "western",
        ],
    ),
    (
        Genre::RnbSoul,
        &[
            "r&b",
            "rnb",
            "r-n-b",
            "rhythm and blues",
            "soul",
            "funk",
            "motown",
            "neo soul",
            "quiet storm",
            "new jack",
        ],
    ),
    (Genre::Blues, &["blues"]),
    (
        Genre::Jazz,
        &["jazz", "bebop", "swing", "big band", "bossa nova", "fusion"],
    ),
    (
        Genre::Classical,
        &[
            "classical",
            "orchestr",
            "baroque",
            "opera",
            "symphon",
            "chamber",
            "romantic era",
            "early music",
            "neoclassical",
            "piano cover",
            "choral",
        ],
    ),
    (
        Genre::HipHop,
        &[
            "hip hop", "hip-hop", "hiphop", "rap", "trap", "drill", "grime", "boom bap", "crunk",
            "phonk",
        ],
    ),
    (
        Genre::Electronic,
        &[
            "electronic",
            "edm",
            "house",
            "techno",
            "trance",
            "dubstep",
            "drum and bass",
            "dnb",
            "jungle",
            "uk garage",
            "synthwave",
            "electro",
            "ambient",
            "idm",
            "breakbeat",
            "hardstyle",
            "eurodance",
            "dance pop",
            "big room",
            "bass music",
            "lo-fi",
            "lofi",
            "chillwave",
            "vaporwave",
            "downtempo",
            "trip hop",
            "disco",
            "nu disco",
        ],
    ),
    (
        Genre::Folk,
        &[
            "folk",
            "singer-songwriter",
            "acoustic",
            "celtic",
            "traditional",
        ],
    ),
    (
        Genre::Indie,
        &[
            "indie",
            "independent",
            "alternative",
            "alt ",
            "shoegaze",
            "dream pop",
            "bedroom",
            "garage rock",
            "post-punk",
            "new wave",
            "britpop",
            "slacker",
        ],
    ),
    (
        Genre::Rock,
        &[
            "rock",
            "grunge",
            "psychedelic",
            "progressive",
            "prog",
            "glam",
            "stoner",
            "post-rock",
            "classic rock",
            "southern rock",
            "surf",
        ],
    ),
    (
        Genre::Pop,
        &[
            "pop",
            "boy band",
            "girl group",
            "idol",
            "schlager",
            "adult standards",
            "easy listening",
            "soft rock",
        ],
    ),
];

/// Classify free-text tags into genres, most frequent first, deduplicated.
pub fn classify<S: AsRef<str>>(tags: &[S]) -> Vec<Genre> {
    let mut counts: Vec<(Genre, usize)> = Vec::new();
    for tag in tags {
        let t = tag.as_ref().to_lowercase().replace('_', " ");
        if let Some(g) = classify_one(&t) {
            match counts.iter_mut().find(|(x, _)| *x == g) {
                Some(entry) => entry.1 += 1,
                None => counts.push((g, 1)),
            }
        }
    }
    // Most votes first; on ties prefer the more specific genre, because YouTube
    // tags almost everything mainstream "Pop_music" and "Independent_music".
    counts.sort_by_key(|&(g, n)| (std::cmp::Reverse(n), std::cmp::Reverse(specificity(g))));
    counts.into_iter().map(|(g, _)| g).collect()
}

/// Tie-break rank: higher is more specific.
fn specificity(g: Genre) -> u8 {
    match g {
        Genre::Pop => 0,
        Genre::Indie => 1,
        Genre::Rock | Genre::Electronic | Genre::HipHop | Genre::RnbSoul | Genre::Folk => 2,
        _ => 3,
    }
}

fn classify_one(tag: &str) -> Option<Genre> {
    RULES
        .iter()
        .find(|(_, needles)| needles.iter().any(|n| tag.contains(n)))
        .map(|(g, _)| *g)
}

/// Several emoji sets per genre; `emojis` chooses one deterministically per track.
fn sets(g: Genre) -> &'static [&'static str] {
    match g {
        Genre::Metal => &["🤘🖤", "⚡🔥🤘", "💀🎸", "🤘🔥🖤"],
        Genre::Punk => &["🧷🎸", "🤘💥", "🛹🎸", "🧷🔥"],
        Genre::Rock => &["🤙🎸", "🤘🎸", "🎸🔥", "🥁🎸⚡", "🤘🥁"],
        Genre::HipHop => &["🎤🔥", "🧢🔊", "💎🎧", "🎙️💯", "🔊🧢🔥"],
        Genre::Electronic => &["🎧🔊", "🪩💫", "⚡🎛️", "🌌🎧", "🪩🔊"],
        Genre::Country => &["🤠🎸", "🪕🌾", "🤠🐎", "🥾🎸"],
        Genre::Folk => &["🪕🌲", "🍂🎻", "🏕️🎸", "🌾🎶"],
        Genre::Jazz => &["🎷🌙", "🎺✨", "🎹🥃", "🎷🕯️"],
        Genre::Blues => &["🎸💙", "🎷🌧️", "🎸🥃", "💙🎶"],
        Genre::Classical => &["🎻🎼", "🎹🏛️", "🎼✨", "🎻🕯️"],
        Genre::RnbSoul => &["💜🎤", "🕯️🎶", "✨🎷", "💋🎶"],
        Genre::Reggae => &["🌴🇯🇲", "☀️🌿", "🌊🥁", "🌴🎶"],
        Genre::Latin => &["💃🔥", "🌶️🎺", "🪇☀️", "💃🕺"],
        Genre::Kpop => &["💜✨", "🌸🎤", "🫰💖", "💫🎤"],
        Genre::Jpop => &["🌸🎌", "🎏🎤", "🌸✨", "🍡🎶"],
        Genre::Indie => &["📼🌿", "🎸☁️", "🌻🎧", "📻🍃"],
        Genre::Christian => &["🙏✨", "⛪🎶", "🕊️🎶"],
        Genre::Soundtrack => &["🎬🎼", "🍿🎶", "🎞️🎻"],
        Genre::Children => &["🧸🎶", "🌈🎵", "🎈🎶"],
        Genre::Holiday => &["🎄🔔", "❄️🎶", "🎅🎵"],
        Genre::Pop => &["✨🎤", "💖🎶", "🎤🌈", "🫧💃", "🎉🎶"],
    }
}

/// FNV-1a over the seed string; stable across builds, unlike `DefaultHasher`.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Emoji set for the top genre, chosen deterministically from `seed` (the
/// source id) so the same song always gets the same set but different songs
/// vary. Empty when no genre is known.
pub fn emojis(genres: &[Genre], seed: &str) -> String {
    let h = fnv1a(seed);
    genres
        .first()
        .map(|g| {
            let s = sets(*g);
            s[(h as usize) % s.len()].to_string()
        })
        .unwrap_or_default()
}

/// Genres for an item plus any extra tags learned from the match.
pub fn for_item<S: AsRef<str>>(item: &Item, extra_tags: &[S]) -> Vec<Genre> {
    let mut tags: Vec<String> = item.genres.clone();
    tags.extend(extra_tags.iter().map(|s| s.as_ref().to_string()));
    classify(&tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotify_style_tags() {
        assert_eq!(classify(&["pop punk", "emo"]), vec![Genre::Punk]);
        assert_eq!(classify(&["indie rock", "garage rock"]), vec![Genre::Indie]);
        assert_eq!(classify(&["k-pop", "k-pop boy group"]), vec![Genre::Kpop]);
        assert_eq!(
            classify(&["melodic rap", "trap", "hip hop"]),
            vec![Genre::HipHop]
        );
        assert_eq!(
            classify(&["contemporary country", "country road"]),
            vec![Genre::Country]
        );
        assert_eq!(
            classify(&["canadian pop", "pop", "dance pop"]),
            vec![Genre::Pop, Genre::Electronic]
        );
        assert_eq!(
            classify(&["nu metal", "alternative metal", "rock"]),
            vec![Genre::Metal, Genre::Rock]
        );
        assert_eq!(classify(&["neo soul", "r&b"]), vec![Genre::RnbSoul]);
        assert_eq!(classify(&["chicago blues"]), vec![Genre::Blues]);
        assert!(classify(&["something unknown"]).is_empty());
    }

    #[test]
    fn youtube_topic_urls() {
        let tags = [
            "https://en.wikipedia.org/wiki/Hip_hop_music",
            "https://en.wikipedia.org/wiki/Music",
        ];
        assert_eq!(classify(&tags), vec![Genre::HipHop]);
        assert_eq!(
            classify(&["https://en.wikipedia.org/wiki/Rhythm_and_blues"]),
            vec![Genre::RnbSoul]
        );
        assert_eq!(
            classify(&["https://en.wikipedia.org/wiki/Music_of_Latin_America"]),
            vec![Genre::Latin]
        );
        assert_eq!(
            classify(&["https://en.wikipedia.org/wiki/Independent_music"]),
            vec![Genre::Indie]
        );
        assert_eq!(
            classify(&["https://en.wikipedia.org/wiki/Electronic_music"]),
            vec![Genre::Electronic]
        );
        assert_eq!(
            classify(&["https://en.wikipedia.org/wiki/Classical_music"]),
            vec![Genre::Classical]
        );
        assert_eq!(
            classify(&["https://en.wikipedia.org/wiki/Rock_music"]),
            vec![Genre::Rock]
        );
        assert_eq!(
            classify(&["https://en.wikipedia.org/wiki/Pop_music"]),
            vec![Genre::Pop]
        );
    }

    #[test]
    fn emojis_are_deterministic_and_vary_by_seed() {
        let a = emojis(&[Genre::Rock], "track-a");
        assert_eq!(a, emojis(&[Genre::Rock], "track-a"));
        assert!(sets(Genre::Rock).contains(&a.as_str()));
        let distinct: std::collections::HashSet<String> = (0..40)
            .map(|i| emojis(&[Genre::Rock], &format!("seed{i}")))
            .collect();
        assert!(distinct.len() > 1, "same set for every seed");
        assert_eq!(emojis(&[], "x"), "");
        assert_eq!(
            emojis(&[Genre::Rock, Genre::Pop, Genre::Jazz], "x"),
            emojis(&[Genre::Rock], "x")
        );
    }
}
