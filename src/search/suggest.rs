//! Invisible search intelligence (Phase 48 slice): typo-tolerant suggestions
//! over popular catalog titles using prefix > substring > edit-distance rank.

use serde::Serialize;
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Suggestion {
    pub title: String,
    pub score: f64,
    pub reason: &'static str,
}

/// Classic DP edit distance, bounded early-exit.
pub fn levenshtein(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }

    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];

    for i in 1..=a.len() {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(curr[j]);
        }
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

pub async fn suggest(pool: &PgPool, query: &str, limit: i64) -> anyhow::Result<Vec<Suggestion>> {
    let q = query.trim().to_lowercase();
    if q.len() < 2 {
        return Ok(Vec::new());
    }

    let titles: Vec<(String,)> =
        sqlx::query_as("SELECT title FROM media ORDER BY popularity DESC LIMIT 500")
            .fetch_all(pool)
            .await?;

    let mut scored: Vec<Suggestion> = Vec::new();
    for (title,) in &titles {
        let t = title.to_lowercase();
        let (score, reason): (f64, &'static str) = if t == q {
            (100.0, "exact")
        } else if t.starts_with(&q) {
            (90.0, "prefix")
        } else if t.contains(&q) {
            if t.split_whitespace().any(|w| w.starts_with(&q)) {
                (80.0, "word-prefix")
            } else {
                (70.0, "substring")
            }
        } else {
            // Typo tolerance scaled to word length.
            let budget = (q.len() / 4).clamp(1, 3);
            let d = levenshtein(&q, &t, budget);
            if d <= budget {
                (60.0 - d as f64 * 8.0, "typo")
            } else {
                continue;
            }
        };

        scored.push(Suggestion {
            title: title.clone(),
            score,
            reason,
        });
    }

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(limit.clamp(1, 10) as usize);
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_basics() {
        assert_eq!(levenshtein("kitten", "sitting", 5), 3);
        assert_eq!(levenshtein("duna", "dune", 3), 1);
        assert_eq!(levenshtein("abc", "abc", 2), 0);
        assert_eq!(levenshtein("aaaa", "bbbb", 2), 3); // early exit over budget
    }

    #[test]
    fn budget_scales_with_length() {
        assert!(levenshtein("interstellar", "interstelar", 3) <= 3);
    }
}
