//! Blender Foundation open movies - the reference plugin.
//!
//! Every film here is Creative Commons, which is what made it the example
//! plugin: a complete, legal source that can be run while learning the
//! protocol. The Python served a fixed list too.

use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};

struct Film {
    id: &'static str,
    title: &'static str,
    year: i64,
    overview: &'static str,
    poster: &'static str,
    stream: &'static str,
    label: &'static str,
}

const FILMS: [Film; 3] = [
    Film {
        id: "big-buck-bunny",
        title: "Big Buck Bunny",
        year: 2008,
        overview: "A large rabbit deals with three rodents who torment smaller creatures of the forest. The Blender Foundation's second open movie, released under CC-BY.",
        poster: "https://upload.wikimedia.org/wikipedia/commons/c/c5/Big_buck_bunny_poster_big.jpg",
        stream: "{base}/bigbuckbunny_movies/BigBuckBunny_320x180.mp4",
        label: "320p",
    },
    Film {
        id: "sintel",
        title: "Sintel",
        year: 2010,
        overview: "A lone girl searches for the dragon she once nursed back to health. Blender's third open movie, released under CC-BY.",
        poster: "https://upload.wikimedia.org/wikipedia/commons/6/64/Sintel_poster.jpg",
        stream: "https://download.blender.org/durian/trailer/sintel_trailer-480p.mp4",
        label: "480p",
    },
    Film {
        id: "tears-of-steel",
        title: "Tears of Steel",
        year: 2012,
        overview: "A group of warriors and scientists gather to save the world from destructive robots. Blender's fourth open movie, CC-BY.",
        poster: "https://upload.wikimedia.org/wikipedia/commons/6/6d/Tears_of_Steel_poster.jpg",
        stream: "https://download.blender.org/demo/movies/ToS/ToS-4k-1920.mov",
        label: "1080p",
    },
];

/// Only the first film's url is built from the CDN base, as in the original.
fn base() -> String {
    std::env::var("OPENMOVIES_BASE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://download.blender.org/peach".into())
}

fn item(film: &Film) -> Value {
    json!({
        "external_id": film.id,
        "title": film.title,
        "year": film.year,
        "overview": film.overview,
        "poster": film.poster,
        "category": "movie",
    })
}

pub fn search(query: &str, limit: usize) -> ApiResult<Value> {
    let needle = query.trim().to_lowercase();
    let hits: Vec<Value> = FILMS
        .iter()
        .filter(|f| needle.is_empty() || f.title.to_lowercase().contains(&needle))
        .take(limit.max(1))
        .map(item)
        .collect();
    Ok(json!({ "items": hits }))
}

pub fn details(external_id: &str) -> ApiResult<Value> {
    let Some(film) = FILMS.iter().find(|f| f.id == external_id) else {
        return Err(ApiError::bad_request(format!("unknown id: {external_id}")));
    };
    Ok(json!({
        "item": item(film),
        "streams": [{
            "url": film.stream.replace("{base}", &base()),
            "kind": "mp4",
            "label": film.label,
        }],
    }))
}
