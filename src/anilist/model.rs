use crate::release_date::ReleaseDate;
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct Anime {
    pub(super) id: u32,
    pub(super) title: AnimeTitle,
    #[serde(rename = "startDate")]
    pub(super) start_date: Option<PartialDate>,
    #[serde(rename = "nextAiringEpisode")]
    pub(crate) next_airing_episode: Option<AiringEpisode>,
    pub(super) relations: Option<AnimeRelations>,
    #[serde(rename = "coverImage")]
    pub(super) cover_image: Option<AnimeCover>,
    #[serde(rename = "airingSchedule")]
    pub(super) airing_schedule: Option<AiringSchedule>,
    #[serde(skip)]
    pub(super) scheduled_episodes: Vec<ScheduledEpisode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnimeCover {
    #[serde(rename = "extraLarge")]
    pub(super) extra_large: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AiringSchedule {
    #[serde(rename = "pageInfo")]
    pub(super) page_info: SchedulePageInfo,
    pub(super) nodes: Vec<ScheduledEpisode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ScheduleMedia {
    #[serde(rename = "airingSchedule")]
    pub(super) airing_schedule: AiringSchedule,
}

#[derive(Debug, Deserialize)]
pub(super) struct SchedulePageInfo {
    #[serde(rename = "hasNextPage")]
    pub(super) has_next_page: bool,
}

#[derive(Debug, PartialEq, Deserialize)]
pub(super) struct ScheduledEpisode {
    pub(super) id: u32,
    pub(super) episode: u32,
    #[serde(rename = "airingAt", with = "chrono::serde::ts_seconds")]
    pub(super) airing_at: DateTime<Utc>,
}

impl Anime {
    pub(super) fn sequels(&self) -> Vec<&Sequel> {
        self.relations
            .iter()
            .flat_map(|relations| &relations.edges)
            .filter(|edge| {
                edge.relation_type == "SEQUEL" && edge.node.media_type.as_deref() == Some("ANIME")
            })
            .map(|edge| &edge.node)
            .collect()
    }

    pub(crate) fn announced_sequels(&self) -> Vec<&Sequel> {
        self.sequels()
            .into_iter()
            .filter(|sequel| sequel.status.as_deref() == Some("NOT_YET_RELEASED"))
            .collect()
    }

    pub(crate) fn title(&self) -> &str {
        self.title
            .english
            .as_deref()
            .or(self.title.romaji.as_deref())
            .unwrap_or("(untitled)")
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct AnimeRelations {
    pub(super) edges: Vec<AnimeRelation>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnimeRelation {
    #[serde(rename = "relationType")]
    pub(super) relation_type: String,
    pub(super) node: Sequel,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Sequel {
    pub(crate) id: u32,
    pub(super) title: AnimeTitle,
    #[serde(rename = "type")]
    pub(super) media_type: Option<String>,
    pub(super) status: Option<String>,
    #[serde(rename = "startDate")]
    pub(super) start_date: Option<PartialDate>,
}

impl Sequel {
    pub(crate) fn title(&self) -> &str {
        self.title
            .english
            .as_deref()
            .or(self.title.romaji.as_deref())
            .unwrap_or("(untitled)")
    }

    pub(crate) fn release_window(&self) -> String {
        match &self.start_date {
            Some(date) => date.display(),
            None => "date unknown".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct PartialDate {
    pub(super) year: Option<i32>,
    pub(super) month: Option<u32>,
    pub(super) day: Option<u32>,
}

impl PartialDate {
    fn display(&self) -> String {
        match (self.year, self.month, self.day) {
            (Some(year), Some(month), Some(day)) => format!("{year:04}-{month:02}-{day:02}"),
            (Some(year), Some(month), None) => format!("{year:04}-{month:02} (day unknown)"),
            (Some(year), None, None) => format!("{year:04} (month and day unknown)"),
            (None, None, None) => "date unknown".to_owned(),
            _ => format!(
                "partial date: year={}, month={}, day={}",
                self.year
                    .map_or("unknown".to_owned(), |value| value.to_string()),
                self.month
                    .map_or("unknown".to_owned(), |value| value.to_string()),
                self.day
                    .map_or("unknown".to_owned(), |value| value.to_string()),
            ),
        }
    }

    pub(super) fn to_release_date(&self) -> Result<Option<ReleaseDate>> {
        match (self.year, self.month, self.day) {
            (None, None, None) => Ok(None),
            (Some(year), Some(month), Some(day)) => {
                let date = NaiveDate::from_ymd_opt(year, month, day).with_context(|| {
                    format!("Invalid AniList start date: {year}-{month:02}-{day:02}")
                })?;

                Ok(Some(ReleaseDate::Date(date)))
            }
            _ => Ok(Some(ReleaseDate::Partial {
                year: self.year,
                month: self.month,
                day: self.day,
            })),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct AnimeTitle {
    pub(super) romaji: Option<String>,
    pub(super) english: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AiringEpisode {
    pub(crate) episode: u32,
    #[serde(rename = "airingAt", with = "chrono::serde::ts_seconds")]
    pub(crate) airing_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anilist::test_support::{FRIEREN_SEASON_2, parse_anime};

    #[test]
    fn parses_anime_without_a_scheduled_episode() {
        let anime = parse_anime(FRIEREN_SEASON_2).unwrap();

        assert_eq!(anime.id, 182255);
        assert_eq!(
            anime.title.romaji.as_deref(),
            Some("Sousou no Frieren 2nd Season")
        );
        assert!(anime.title.english.is_none());
        assert_eq!(anime.title(), "Sousou no Frieren 2nd Season");
        assert!(anime.next_airing_episode.is_none());
    }

    #[test]
    fn parses_an_episode_airing_time() {
        let json = serde_json::json!({ "data": { "Media": {
            "id": 1,
            "title": {},
            "nextAiringEpisode": { "episode": 1, "airingAt": 0 },
        }}});

        let anime = parse_anime(&json.to_string()).unwrap();
        let episode = anime.next_airing_episode.unwrap();

        assert_eq!(episode.episode, 1);
        assert_eq!(
            episode.airing_at,
            "1970-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn ignores_non_sequels_manga_and_finished_seasons() {
        let mut edges = Vec::new();
        for (relation, media_type, status) in [
            ("SIDE_STORY", "ANIME", "NOT_YET_RELEASED"),
            ("SEQUEL", "MANGA", "NOT_YET_RELEASED"),
            ("SEQUEL", "ANIME", "FINISHED"),
            ("SEQUEL", "ANIME", "NOT_YET_RELEASED"),
        ] {
            edges.push(serde_json::json!({
                "relationType": relation,
                "node": {
                    "id": 1,
                    "type": media_type,
                    "status": status,
                    "title": {"english": "Example"}
                }
            }));
        }
        let response = serde_json::json!({
            "data": {"Media": {
                "id": 1,
                "title": {},
                "relations": {"edges": edges}
            }}
        });
        let anime = parse_anime(&response.to_string()).unwrap();
        let sequels = anime.announced_sequels();
        assert_eq!(sequels.len(), 1);
        assert_eq!(sequels[0].release_window(), "date unknown");
    }

    #[test]
    fn preserves_partial_release_dates() {
        for (json, expected) in [
            (r#"{"year":2027,"month":10,"day":12}"#, "2027-10-12"),
            (
                r#"{"year":2027,"month":10,"day":null}"#,
                "2027-10 (day unknown)",
            ),
            (r#"{"year":2027}"#, "2027 (month and day unknown)"),
            ("{}", "date unknown"),
        ] {
            let date: PartialDate = serde_json::from_str(json).unwrap();
            assert_eq!(date.display(), expected);
        }
    }

    #[test]
    fn converts_an_announced_month_without_inventing_a_day() {
        let date = PartialDate {
            year: Some(2027),
            month: Some(10),
            day: None,
        };

        assert_eq!(
            date.to_release_date().unwrap(),
            Some(ReleaseDate::Partial {
                year: Some(2027),
                month: Some(10),
                day: None,
            })
        );
    }

    #[test]
    fn rejects_an_invalid_complete_start_date() {
        let date = PartialDate {
            year: Some(2027),
            month: Some(2),
            day: Some(30),
        };

        assert!(date.to_release_date().is_err());
    }
}
