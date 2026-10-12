use std::sync::LazyLock;

use regex::Regex;

static SEASON_EPISODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bs(\d{1,3})[\s._-]*e(\d{1,4})\b").expect("s01e02 regex"));
static EPISODE_ONLY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|[\s._-])e(?:p)?(\d{1,4})\b").expect("ep regex"));
static YEAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(19\d{2}|20\d{2})\b").expect("year regex"));

const NOISE_TOKENS: &[&str] = &[
    "1080p",
    "720p",
    "480p",
    "2160p",
    "4k",
    "webrip",
    "web-dl",
    "webdl",
    "web",
    "hdtv",
    "bluray",
    "blu-ray",
    "bdrip",
    "dvdrip",
    "brrip",
    "x264",
    "x265",
    "h264",
    "h.264",
    "hevc",
    "avc",
    "aac",
    "aac2",
    "ac3",
    "eac3",
    "ddp5",
    "ddp",
    "atmos",
    "10bit",
    "hdr",
    "dv",
    "yify",
    "rarbg",
    "eztv",
    "ettv",
    "fgt",
    "nfh",
    "amzn",
    "nf",
    "dsnp",
    "hmax",
    "ita",
    "eng",
    "jpn",
    "multi",
    "subbed",
    "dubbed",
    "dual",
    "audio",
    "proper",
    "repack",
    "extended",
    "remastered",
    "cut",
];

fn clean_title(raw: &str) -> String {
    let mut title = raw.to_string();
    if let Some(pos) = title.find(|c: char| c == '(') {
        title.truncate(pos);
    }
    let cleaned: String = title
        .split(|c: char| c == '.' || c == '_' || c == '-' || c == ' ')
        .filter(|token| !token.is_empty())
        .filter(|token| {
            let lower = token.to_ascii_lowercase();
            !NOISE_TOKENS.contains(&lower.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ");

    cleaned.trim().to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFileName {
    pub title: String,
    pub year: Option<i32>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
}

impl ParsedFileName {
    #[allow(dead_code)]
    pub fn is_series(&self) -> bool {
        self.season.is_some() || self.episode.is_some()
    }
}

pub fn parse(file_stem: &str) -> ParsedFileName {
    let stem = file_stem.trim();

    let mut season = None;
    let mut episode = None;

    if let Some(caps) = SEASON_EPISODE_RE.captures(stem) {
        season = caps.get(1).and_then(|m| m.as_str().parse().ok());
        episode = caps.get(2).and_then(|m| m.as_str().parse().ok());
    }

    if episode.is_none() {
        if let Some(caps) = EPISODE_ONLY_RE.captures(stem) {
            episode = caps.get(1).and_then(|m| m.as_str().parse().ok());
            if season.is_none() && episode.unwrap_or(0) <= 99 {
                season = Some(1);
            }
        }
    }

    let year = YEAR_RE
        .captures(stem)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse().ok());

    let episode_only_matched = episode.is_some() && season.is_none();
    let cut_positions = [
        SEASON_EPISODE_RE.find(stem).map(|m| m.start()),
        if episode_only_matched {
            EPISODE_ONLY_RE.find(stem).map(|m| m.start())
        } else {
            None
        },
        YEAR_RE
            .find(stem)
            .filter(|_| season.is_none())
            .map(|m| m.start()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    let title_source = match cut_positions.iter().min() {
        Some(pos) => &stem[..*pos],
        None => stem,
    };

    let mut title = clean_title(title_source);
    if title.is_empty() {
        title = clean_title(stem);
    }

    ParsedFileName {
        title,
        year,
        season,
        episode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_series_names() {
        let parsed = parse("Attack.on.Titan.S04E28.1080p.WEB-DL.x264-GROUP");
        assert_eq!(parsed.title, "Attack on Titan");
        assert_eq!(parsed.season, Some(4));
        assert_eq!(parsed.episode, Some(28));
        assert!(parsed.is_series());
    }

    #[test]
    fn parses_movies_with_year() {
        let parsed = parse("Spirited.Away.2001.1080p.BluRay.x264");
        assert_eq!(parsed.title, "Spirited Away");
        assert_eq!(parsed.year, Some(2001));
        assert_eq!(parsed.season, None);
        assert!(!parsed.is_series());
    }

    #[test]
    fn parses_episode_only() {
        let parsed = parse("Show.Name.E07.720p");
        assert_eq!(parsed.season, Some(1));
        assert_eq!(parsed.episode, Some(7));
    }

    #[test]
    fn falls_back_to_full_name() {
        let parsed = parse("My Cool Movie");
        assert_eq!(parsed.title, "My Cool Movie");
    }
}
