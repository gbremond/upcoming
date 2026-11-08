//! Shared AniList test data. The JSON payloads live in `fixtures/` and are
//! embedded at compile time.

use super::client::parse_media;
use super::model::Anime;

/// Frieren season 2: aired, with a cover and an announced season 3 sequel.
pub(super) const FRIEREN_SEASON_2: &str = include_str!("fixtures/frieren_season_2.json");

/// Frieren season 3: announced for October 2027, day unknown, no cover yet.
pub(super) const FRIEREN_SEASON_3_ANNOUNCED: &str =
    include_str!("fixtures/frieren_season_3_announced.json");

/// The GraphQL error body AniList returns when rate limited.
pub(super) const TOO_MANY_REQUESTS: &str = include_str!("fixtures/too_many_requests.json");

pub(super) fn parse_anime(json: &str) -> anyhow::Result<Anime> {
    parse_media(json)
}
