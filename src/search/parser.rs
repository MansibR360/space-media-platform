use crate::catalog::models::MediaType;
use serde::{Deserialize, Serialize};

/// Turns free-form queries like `action 2020-2025 1080p english` into
/// structured filters. Pure Rust, deterministic, unit-tested.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ParsedQuery {
    /// leftover keywords matched against title/overview
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub genre_slugs: Vec<String>,
    #[serde(default)]
    pub media_type: Option<MediaType>,
    #[serde(default)]
    pub year_from: Option<i32>,
    #[serde(default)]
    pub year_to: Option<i32>,
    /// 480p / 720p / 1080p / 2160p
    #[serde(default)]
    pub min_resolution: Option<i32>,
    #[serde(default)]
    pub language: Option<String>,
}

const GENRE_WORDS: &[(&str, &str)] = &[
    ("action", "action"),
    ("adventure", "adventure"),
    ("animation", "animation"),
    ("anime", "anime"),
    ("comedy", "comedy"),
    ("crime", "crime"),
    ("documentary", "documentary"),
    ("drama", "drama"),
    ("family", "family"),
    ("fantasy", "fantasy"),
    ("history", "history"),
    ("horror", "horror"),
    ("music", "music"),
    ("mystery", "mystery"),
    ("romance", "romance"),
    ("scifi", "science-fiction"),
    ("sci-fi", "science-fiction"),
    ("thriller", "thriller"),
    ("war", "war"),
    ("western", "western"),
];

const LANGUAGE_WORDS: &[(&str, &str)] = &[
    ("english", "en"),
    ("japanese", "ja"),
    ("german", "de"),
    ("french", "fr"),
    ("spanish", "es"),
    ("korean", "ko"),
    ("chinese", "zh"),
    ("italian", "it"),
];

const RESOLUTIONS: &[(&str, i32)] = &[
    ("480p", 480),
    ("576p", 576),
    ("720p", 720),
    ("1080p", 1080),
    ("1440p", 1440),
    ("2160p", 2160),
    ("4k", 2160),
    ("8k", 4320),
];

pub fn parse(raw: &str) -> ParsedQuery {
    let mut out = ParsedQuery::default();
    let lower = raw.to_lowercase();

    // Year ranges: "2020-2025", "since 2019", "after 2000", "before 2010"
    for token in lower.split(|c: char| !c.is_alphanumeric() && c != '-') {
        if token.is_empty() {
            continue;
        }
        if let Some((a, b)) = token.split_once('-') {
            if let (Ok(y1), Ok(y2)) = (a.parse::<i32>(), b.parse::<i32>()) {
                if (1900..=2100).contains(&y1) && (1900..=2100).contains(&y2) && y1 <= y2 {
                    out.year_from = Some(y1);
                    out.year_to = Some(y2);
                    continue;
                }
            }
        }
        if let Some(rest) = token
            .strip_prefix("since")
            .or_else(|| token.strip_prefix("after"))
        {
            if let Ok(y) = rest.trim().parse::<i32>() {
                out.year_from = Some(y);
                continue;
            }
        }
        if let Some(rest) = token.strip_prefix("before") {
            if let Ok(y) = rest.trim().parse::<i32>() {
                out.year_to = Some(y - 1);
                continue;
            }
        }
        if token.len() == 3 && token.ends_with('s') {
            if let Ok(decade) = token[..2].parse::<i32>() {
                let century_base = if raw.len() > 3 { 1900 } else { 1900 };
                let _ = century_base;
                let start = 1900 + decade * 10 + if decade >= 0 { 0 } else { 0 };
                let _ = start;
            }
        }
        // "90s" style decades
        if let Some(decade_str) = token.strip_suffix('s') {
            if let Ok(d) = decade_str.parse::<i32>() {
                if (30..=99).contains(&d) {
                    out.year_from = Some(1900 + d);
                    out.year_to = Some(1900 + d + 9);
                    continue;
                } else if (0..=29).contains(&d) {
                    out.year_from = Some(2000 + d);
                    out.year_to = Some(2000 + d + 9);
                    continue;
                }
            }
        }

        // Resolution tokens
        for (word, px) in RESOLUTIONS {
            if token == *word {
                out.min_resolution = Some(out.min_resolution.map_or(*px, |cur| cur.max(*px)));
            }
        }

        // Languages
        for (word, code) in LANGUAGE_WORDS {
            if token == *word {
                out.language = Some(code.to_string());
            }
        }

        // Genres
        for (word, slug) in GENRE_WORDS {
            if token == *word {
                if *word == "anime" {
                    continue; // handled as flag below via genre too — keep both
                }
                if !out.genre_slugs.iter().any(|g| g == slug) {
                    out.genre_slugs.push(slug.to_string());
                }
            }
        }
        if token == "anime" {
            out.media_type = Some(MediaType::Series);
            if !out.genre_slugs.iter().any(|g| g == "animation") {
                out.genre_slugs.push("animation".into());
            }
        }
        if token == "movies" || token == "movie" || token == "film" {
            out.media_type = Some(MediaType::Movie);
        }
        if token == "series" || token == "shows" || token == "tv" {
            out.media_type = Some(MediaType::Series);
        }
        if token == "books" || token == "book" || token == "ebook" || token == "novel" {
            out.media_type = Some(MediaType::Book);
        }
        if token == "podcasts" || token == "podcast" {
            out.media_type = Some(MediaType::Podcast);
        }
        if token == "audiobooks" || token == "audiobook" {
            out.media_type = Some(MediaType::Audiobook);
        }
    }

    // Keywords = original tokens that were not consumed by any rule.
    let consumed_lower = build_consumed_signature(&out);
    for word in lower.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric());
        if cleaned.is_empty() || consumed_lower.iter().any(|w| w == cleaned) {
            continue;
        }
        if is_rule_word(cleaned) {
            continue;
        }
        // Numeric-led tokens were already interpreted as years / ranges /
        // resolutions (e.g. `2020`, `2020-2025`, `1080p`).
        if cleaned.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        out.keywords.push(cleaned.to_string());
    }

    out
}

fn build_consumed_signature(_pq: &ParsedQuery) -> Vec<String> {
    let mut words = Vec::new();
    for (word, _) in GENRE_WORDS.iter().chain(LANGUAGE_WORDS.iter()) {
        words.push(word.to_string());
    }
    for (word, _) in RESOLUTIONS.iter() {
        words.push(word.to_string());
    }
    words.extend([
        "since".to_string(),
        "after".to_string(),
        "before".to_string(),
        "movies".to_string(),
        "movie".to_string(),
        "film".to_string(),
        "series".to_string(),
        "shows".to_string(),
        "tv".to_string(),
        "books".to_string(),
        "book".to_string(),
        "ebook".to_string(),
        "novel".to_string(),
    ]);
    words
}

fn is_rule_word(word: &str) -> bool {
    matches!(
        word,
        "action"
            | "adventure"
            | "comedy"
            | "crime"
            | "documentary"
            | "drama"
            | "family"
            | "fantasy"
            | "history"
            | "horror"
            | "music"
            | "mystery"
            | "romance"
            | "scifi"
            | "sci-fi"
            | "thriller"
            | "war"
            | "western"
            | "english"
            | "japanese"
            | "german"
            | "french"
            | "spanish"
            | "korean"
            | "chinese"
            | "italian"
            | "480p"
            | "720p"
            | "1080p"
            | "1440p"
            | "2160p"
            | "4k"
            | "8k"
            | "anime"
            | "movies"
            | "movie"
            | "film"
            | "series"
            | "shows"
            | "tv"
            | "books"
            | "book"
            | "ebook"
            | "novel"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_canonical_example() {
        let pq = parse("action 2020-2025 1080p english");
        assert_eq!(pq.genre_slugs, vec!["action"]);
        assert_eq!(pq.year_from, Some(2020));
        assert_eq!(pq.year_to, Some(2025));
        assert_eq!(pq.min_resolution, Some(1080));
        assert_eq!(pq.language.as_deref(), Some("en"));
        assert!(pq.keywords.is_empty(), "{:?}", pq.keywords);
    }

    #[test]
    fn parses_decades_and_types() {
        let pq = parse("90s comedy movies");
        assert_eq!(pq.year_from, Some(1990));
        assert_eq!(pq.year_to, Some(1999));
        assert!(pq.genre_slugs.contains(&"comedy".to_string()));
        assert_eq!(pq.media_type, Some(MediaType::Movie));

        let books = parse("ebook novel");
        assert_eq!(books.media_type, Some(MediaType::Book));
    }

    #[test]
    fn keeps_free_keywords() {
        let pq = parse("dark knight 1080p");
        assert_eq!(pq.min_resolution, Some(1080));
        assert!(pq.keywords.contains(&"dark".to_string()));
        assert!(pq.keywords.contains(&"knight".to_string()));
    }

    #[test]
    fn handles_plain_text_gracefully() {
        let pq = parse("attack on titan");
        assert!(pq.year_from.is_none());
        assert!(pq.genre_slugs.is_empty());
        assert_eq!(pq.keywords.len(), 3);
    }
}
