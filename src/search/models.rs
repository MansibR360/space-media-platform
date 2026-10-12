use crate::catalog::models::Media;
use serde::{Deserialize, Serialize};
use utoipa::IntoParams;

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct SearchHit {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub media: Media,
    pub score: f64,
}

#[derive(Debug, Deserialize, IntoParams, utoipa::ToSchema)]
pub struct SearchQuery {
    pub q: String,
    // NOT #[serde(flatten)]: serde_urlencoded hands flattened fields through as
    // strings, so `?limit=40` fails with "invalid type: string \"40\", expected
    // i64". Same trap as ListMediaQuery. Inlined, with `page()` rebuilding the
    // struct so call sites keep working.
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl SearchQuery {
    pub fn page(&self) -> crate::pagination::PageQuery {
        crate::pagination::PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrendWindow {
    Day,
    Week,
    Month,
}

impl TrendWindow {
    #[allow(dead_code)]
    pub fn interval(self) -> &'static str {
        match self {
            Self::Day => "1 day",
            Self::Week => "7 days",
            Self::Month => "30 days",
        }
    }
}
