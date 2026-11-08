//! AniList provider: fetches anime seasons and episodes and turns them into
//! tracked releases.

mod client;
mod model;
mod snapshot;
#[cfg(test)]
mod test_support;

pub(crate) use client::AniListClient;
pub(crate) use model::Anime;
pub(crate) use snapshot::anime_snapshot;
