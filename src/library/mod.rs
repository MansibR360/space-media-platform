//! Library playlists: user-saved ordered media collections.
//!
//! - `models` — playlist/item DTOs (`multimedia` vs `standard` flavors)
//! - `store`  — persistence SPI every backend implements + media-only guard
//! - `repo`   — default PostgreSQL backend
//! - `routes` / `handlers` — HTTP surface
//!
//! Playlists hold catalog-media references only; outside text is rejected at
//! the SPI boundary (see [`store::ensure_media_only`]).

pub mod handlers;
pub mod models;
pub mod repo;
pub mod routes;
pub mod store;
