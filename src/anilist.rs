use crate::release_date::ReleaseDate;
use crate::state::{ReleaseRecord, SeriesSnapshot};
use chrono::NaiveDate;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};

const ANIME_QUERY: &str = r#"
query ($id: Int!) {
    Media(id: $id, type: ANIME) {
        id
        title { romaji english }
        startDate { year month day }
        nextAiringEpisode { episode airingAt }
        coverImage { medium }
        airingSchedule(page: 1, perPage: 25) {
            pageInfo { hasNextPage }
            nodes { id episode airingAt }
        }
        relations {
            edges {
                relationType
                node {
                    id
                    type
                    title { romaji english }
                    status
                    startDate { year month day }
                }
            }
        }
    }
}
"#;

const SCHEDULE_QUERY: &str = r#"
query ($id: Int!, $page: Int!) {
    Media(id: $id, type: ANIME) {
        airingSchedule(page: $page, perPage: 25) {
            pageInfo { hasNextPage }
            nodes { id episode airingAt }
        }
    }
}
"#;

pub(crate) struct AniListClient {
    http: reqwest::blocking::Client,
    endpoint: String,
}

impl AniListClient {
    pub(crate) fn new(http: reqwest::blocking::Client, endpoint: String) -> Self {
        Self { http, endpoint }
    }

    pub(crate) fn fetch_anime(&self, id: i32) -> Result<Anime, Box<dyn std::error::Error>> {
        let body = serde_json::json!({
            "query": ANIME_QUERY,
            "variables": { "id": id }
        });

        let mut anime: Anime = self.request_media(body)?;
        let first_page = anime.airing_schedule.take().ok_or_else(|| {
            std::io::Error::other("AniList response is missing the airing schedule")
        })?;
        anime.scheduled_episodes = self.fetch_remaining_schedule(id, first_page)?;
        Ok(anime)
    }

    fn request_media<T: serde::de::DeserializeOwned>(
        &self,
        body: serde_json::Value,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let response = self
            .http
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()?
            .error_for_status()?
            .text()?;

        parse_media(&response)
    }

    fn fetch_remaining_schedule(
        &self,
        id: i32,
        mut schedule: AiringSchedule,
    ) -> Result<Vec<ScheduledEpisode>, Box<dyn std::error::Error>> {
        let mut episodes = std::collections::BTreeMap::new();
        let mut page = 1;

        loop {
            for episode in schedule.nodes {
                if let Some(previous) = episodes.get(&episode.id) {
                    if previous != &episode {
                        return Err(std::io::Error::other(
                            "AniList returned conflicting duplicate airing events",
                        )
                        .into());
                    }
                } else {
                    episodes.insert(episode.id, episode);
                }
            }

            if !schedule.page_info.has_next_page {
                break;
            }
            if page >= 100 {
                return Err(
                    std::io::Error::other("AniList airing schedule exceeded 100 pages").into(),
                );
            }
            page += 1;
            let media: ScheduleMedia = self.request_media(serde_json::json!({
                "query": SCHEDULE_QUERY,
                "variables": { "id": id, "page": page }
            }))?;
            schedule = media.airing_schedule;
        }

        let mut episodes: Vec<_> = episodes.into_values().collect();
        episodes.sort_by_key(|episode| (episode.airing_at, episode.id));
        Ok(episodes)
    }

    pub(crate) fn fetch_sequel_chain(
        &self,
        root_id: i32,
    ) -> Result<Vec<Anime>, Box<dyn std::error::Error>> {
        if root_id <= 0 {
            return Err(std::io::Error::other("AniList ID must be positive").into());
        }

        let mut pending = VecDeque::from([root_id]);
        let mut visited = HashSet::new();
        let mut seasons = Vec::new();

        while let Some(id) = pending.pop_front() {
            if visited.contains(&id) {
                continue;
            }

            if visited.len() >= 20 {
                return Err(
                    std::io::Error::other("AniList sequel traversal exceeded 20 entries").into(),
                );
            }

            visited.insert(id);
            let anime = self.fetch_anime(id)?;

            for sequel in anime.sequels() {
                let sequel_id = i32::try_from(sequel.id)?;

                if !visited.contains(&sequel_id) {
                    pending.push_back(sequel_id);
                }
            }

            seasons.push(anime);
        }

        Ok(seasons)
    }
}

fn parse_anime(contents: &str) -> Result<Anime, Box<dyn std::error::Error>> {
    parse_media(contents)
}

fn parse_media<T: serde::de::DeserializeOwned>(
    contents: &str,
) -> Result<T, Box<dyn std::error::Error>> {
    let response: AniListResponse<T> = serde_json::from_str(contents)?;

    if let Some(errors) = response.errors {
        if !errors.is_empty() {
            let messages = errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ");

            return Err(
                std::io::Error::other(format!("AniList GraphQL errors: {messages}")).into(),
            );
        }
    }

    let data = response
        .data
        .ok_or_else(|| std::io::Error::other("AniList response is missing data"))?;

    let anime = data
        .media
        .ok_or_else(|| std::io::Error::other("AniList response contains no anime"))?;

    Ok(anime)
}

pub(crate) fn anime_snapshot(
    seasons: &[Anime],
) -> Result<SeriesSnapshot, Box<dyn std::error::Error>> {
    let mut releases = std::collections::BTreeMap::new();

    for season in seasons {
        releases.insert(format!("season:{}", season.id), season.release_record()?);

        for episode in &season.scheduled_episodes {
            releases.insert(
                format!("episode:{}", episode.id),
                episode.release_record(season),
            );
        }
    }

    Ok(SeriesSnapshot { releases })
}

#[derive(Debug, Deserialize)]
struct AniListResponse<T> {
    data: Option<AniListData<T>>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct AniListData<T> {
    #[serde(rename = "Media")]
    media: Option<T>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Anime {
    id: u32,
    title: AnimeTitle,
    #[serde(rename = "startDate")]
    start_date: Option<PartialDate>,
    #[serde(rename = "nextAiringEpisode")]
    pub(crate) next_airing_episode: Option<AiringEpisode>,
    relations: Option<AnimeRelations>,
    #[serde(rename = "coverImage")]
    cover_image: Option<AnimeCover>,
    #[serde(rename = "airingSchedule")]
    airing_schedule: Option<AiringSchedule>,
    #[serde(skip)]
    scheduled_episodes: Vec<ScheduledEpisode>,
}

#[derive(Debug, Deserialize)]
struct AnimeCover {
    medium: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AiringSchedule {
    #[serde(rename = "pageInfo")]
    page_info: SchedulePageInfo,
    nodes: Vec<ScheduledEpisode>,
}

#[derive(Debug, Deserialize)]
struct ScheduleMedia {
    #[serde(rename = "airingSchedule")]
    airing_schedule: AiringSchedule,
}

#[derive(Debug, Deserialize)]
struct SchedulePageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
}

#[derive(Debug, PartialEq, Deserialize)]
struct ScheduledEpisode {
    id: u32,
    episode: u32,
    #[serde(rename = "airingAt", with = "chrono::serde::ts_seconds")]
    airing_at: DateTime<Utc>,
}

impl Anime {
    fn sequels(&self) -> Vec<&Sequel> {
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

    fn release_record(&self) -> Result<ReleaseRecord, Box<dyn std::error::Error>> {
        let date = match &self.start_date {
            Some(date) => date.to_release_date()?,
            None => None,
        };

        let title = self
            .title
            .english
            .as_ref()
            .or(self.title.romaji.as_ref())
            .cloned();

        Ok(ReleaseRecord {
            date,
            title,
            image_url: self
                .cover_image
                .as_ref()
                .and_then(|cover| cover.medium.clone()),
            url: Some(format!("https://anilist.co/anime/{}", self.id)),
        })
    }
}

#[derive(Debug, Deserialize)]
struct AnimeRelations {
    edges: Vec<AnimeRelation>,
}

#[derive(Debug, Deserialize)]
struct AnimeRelation {
    #[serde(rename = "relationType")]
    relation_type: String,
    node: Sequel,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Sequel {
    pub(crate) id: u32,
    title: AnimeTitle,
    #[serde(rename = "type")]
    media_type: Option<String>,
    status: Option<String>,
    #[serde(rename = "startDate")]
    start_date: Option<PartialDate>,
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
struct PartialDate {
    year: Option<i32>,
    month: Option<u32>,
    day: Option<u32>,
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

    fn to_release_date(&self) -> Result<Option<ReleaseDate>, Box<dyn std::error::Error>> {
        match (self.year, self.month, self.day) {
            (None, None, None) => Ok(None),
            (Some(year), Some(month), Some(day)) => {
                let date = NaiveDate::from_ymd_opt(year, month, day)
                    .ok_or_else(|| std::io::Error::other("Invalid AniList start date"))?;

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
struct AnimeTitle {
    romaji: Option<String>,
    english: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AiringEpisode {
    pub(crate) episode: u32,
    #[serde(rename = "airingAt", with = "chrono::serde::ts_seconds")]
    pub(crate) airing_at: DateTime<Utc>,
}

impl ScheduledEpisode {
    fn release_record(&self, season: &Anime) -> ReleaseRecord {
        ReleaseRecord {
            date: Some(ReleaseDate::Instant(self.airing_at)),
            title: Some(format!("{} — Episode {}", season.title(), self.episode)),
            image_url: season
                .cover_image
                .as_ref()
                .and_then(|cover| cover.medium.clone()),
            url: Some(format!("https://anilist.co/anime/{}", season.id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetches_all_schedule_pages_and_deduplicates_events() {
        use wiremock::matchers::{body_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let shared = serde_json::json!({"id": 2, "episode": 2, "airingAt": 200});
        for (query, variables, media) in [
            (
                ANIME_QUERY,
                serde_json::json!({"id": 1}),
                serde_json::json!({
                    "id": 1, "title": {},
                    "airingSchedule": {
                        "pageInfo": {"hasNextPage": true},
                        "nodes": [shared.clone()]
                    }
                }),
            ),
            (
                SCHEDULE_QUERY,
                serde_json::json!({"id": 1, "page": 2}),
                serde_json::json!({
                    "airingSchedule": {
                        "pageInfo": {"hasNextPage": false},
                        "nodes": [shared, {"id": 1, "episode": 1, "airingAt": 100}]
                    }
                }),
            ),
        ] {
            Mock::given(method("POST"))
                .and(body_json(serde_json::json!({
                    "query": query, "variables": variables
                })))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"data": {"Media": media}})),
                )
                .expect(1)
                .mount(&server)
                .await;
        }
        let endpoint = server.uri();
        let anime = tokio::task::spawn_blocking(move || {
            let http = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap();
            AniListClient::new(http, endpoint).fetch_anime(1).unwrap()
        })
        .await
        .unwrap();

        assert_eq!(
            anime
                .scheduled_episodes
                .iter()
                .map(|episode| episode.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn schedule_page_failure_does_not_return_partial_anime() {
        use wiremock::matchers::{body_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(serde_json::json!({
                "query": ANIME_QUERY, "variables": {"id": 1}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {"Media": {
                    "id": 1, "title": {},
                    "airingSchedule": {
                        "pageInfo": {"hasNextPage": true},
                        "nodes": [{"id": 1, "episode": 1, "airingAt": 0}]
                    }
                }}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_json(serde_json::json!({
                "query": SCHEDULE_QUERY, "variables": {"id": 1, "page": 2}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "errors": [{"message": "Too Many Requests."}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let endpoint = server.uri();
        let error = tokio::task::spawn_blocking(move || {
            let http = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap();
            AniListClient::new(http, endpoint)
                .fetch_anime(1)
                .unwrap_err()
                .to_string()
        })
        .await
        .unwrap();
        assert_eq!(error, "AniList GraphQL errors: Too Many Requests.");
        server.verify().await;
    }

    #[tokio::test]
    async fn fetches_anime_using_graphql() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(serde_json::json!({
                "query": ANIME_QUERY,
                "variables": { "id": 182255 }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "Media": {
                        "id": 182255,
                        "title": {
                            "romaji": "Sousou no Frieren 2nd Season",
                            "english": null
                        },
                        "nextAiringEpisode": null,
                        "airingSchedule": {
                            "pageInfo": {"hasNextPage": false},
                            "nodes": []
                        },
                        "relations": {
                            "edges": [{
                                "relationType": "SEQUEL",
                                "node": {
                                    "id": 209939,
                                    "type": "ANIME",
                                    "title": {
                                        "romaji": "Sousou no Frieren 3rd Season",
                                        "english": null
                                    },
                                    "status": "NOT_YET_RELEASED",
                                    "startDate": {"year": 2027, "month": 10, "day": null}
                                }
                            }]
                        }
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let endpoint = format!("{}/graphql", server.uri());

        let anime = tokio::task::spawn_blocking(move || {
            let http = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap();

            let client = AniListClient::new(http, endpoint);
            client.fetch_anime(182255).unwrap()
        })
        .await
        .unwrap();

        assert_eq!(anime.id, 182255);
        assert!(anime.next_airing_episode.is_none());
        let sequels = anime.announced_sequels();
        assert_eq!(sequels.len(), 1);
        assert_eq!(sequels[0].id, 209939);
        assert_eq!(sequels[0].title(), "Sousou no Frieren 3rd Season");
        assert_eq!(sequels[0].release_window(), "2027-10 (day unknown)");
        server.verify().await;
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
    fn parses_anime_without_a_scheduled_episode() {
        let json = r#"{
            "data": {
                "Media": {
                    "id": 154587,
                    "title": {
                        "romaji": "Sousou no Frieren",
                        "english": null
                    },
                    "nextAiringEpisode": null
                }
            }
        }"#;

        let anime = parse_anime(json).unwrap();

        assert_eq!(anime.id, 154587);
        assert_eq!(anime.title.romaji.as_deref(), Some("Sousou no Frieren"));
        assert!(anime.title.english.is_none());
        assert!(anime.next_airing_episode.is_none());
    }

    #[test]
    fn reports_graphql_errors() {
        let json = r#"{
        "data": null,
        "errors": [{"message": "Too Many Requests."}]
    }"#;

        let error = parse_anime(json).unwrap_err();

        assert_eq!(
            error.to_string(),
            "AniList GraphQL errors: Too Many Requests."
        );
    }

    #[test]
    fn rejects_missing_data() {
        let error = parse_anime("{}").unwrap_err();

        assert_eq!(error.to_string(), "AniList response is missing data");
    }

    #[test]
    fn rejects_missing_anime() {
        let error = parse_anime(r#"{"data":{"Media":null}}"#).unwrap_err();

        assert_eq!(error.to_string(), "AniList response contains no anime");
    }

    #[test]
    fn parses_an_episode_airing_time() {
        let json = r#"{
        "data": {
            "Media": {
                "id": 154587,
                "title": {"romaji": "Example", "english": null},
                "nextAiringEpisode": {
                    "episode": 1,
                    "airingAt": 0
                }
            }
        }
    }"#;

        let anime = parse_anime(json).unwrap();
        let episode = anime.next_airing_episode.unwrap();

        assert_eq!(episode.episode, 1);
        assert_eq!(
            episode.airing_at,
            "1970-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[tokio::test]
    async fn follows_sequels_without_fetching_an_entry_twice() {
        use wiremock::matchers::{body_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        for (id, sequel_id) in [(1, 2), (2, 3), (3, 1)] {
            Mock::given(method("POST"))
                .and(body_json(serde_json::json!({
                    "query": ANIME_QUERY,
                    "variables": { "id": id }
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "data": {
                        "Media": {
                            "id": id,
                            "title": { "romaji": format!("Season {id}") },
                            "nextAiringEpisode": null,
                            "airingSchedule": {
                                "pageInfo": {"hasNextPage": false},
                                "nodes": []
                            },
                            "relations": {
                                "edges": [{
                                    "relationType": "SEQUEL",
                                    "node": {
                                        "id": sequel_id,
                                        "type": "ANIME",
                                        "title": {},
                                        "status": "FINISHED"
                                    }
                                }]
                            }
                        }
                    }
                })))
                .expect(1)
                .mount(&server)
                .await;
        }

        let endpoint = server.uri();

        let seasons = tokio::task::spawn_blocking(move || {
            let http = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap();

            let client = AniListClient::new(http, endpoint);
            client.fetch_sequel_chain(1).unwrap()
        })
        .await
        .unwrap();

        let ids: Vec<u32> = seasons.iter().map(|anime| anime.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);

        server.verify().await;
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

    #[test]
    fn season_record_preserves_an_announced_month() {
        let anime = parse_anime(
            r#"{
        "data": {
            "Media": {
                "id": 209939,
                "title": {
                    "romaji": "Sousou no Frieren 3rd Season",
                    "english": null
                },
                "startDate": {
                    "year": 2027,
                    "month": 10,
                    "day": null
                }
            }
        }
    }"#,
        )
        .unwrap();

        let record = anime.release_record().unwrap();

        assert_eq!(
            record.date,
            Some(ReleaseDate::Partial {
                year: Some(2027),
                month: Some(10),
                day: None,
            })
        );
        assert_eq!(
            record.title.as_deref(),
            Some("Sousou no Frieren 3rd Season")
        );
        assert_eq!(
            record.url.as_deref(),
            Some("https://anilist.co/anime/209939")
        );
        assert!(record.image_url.is_none());
    }

    #[test]
    fn snapshot_contains_each_season() {
        let seasons: Vec<Anime> = serde_json::from_value(serde_json::json!([
            {
                "id": 182255,
                "title": {"english": "Frieren Season 2"},
                "startDate": {"year": 2026, "month": 1, "day": 16}
            },
            {
                "id": 209939,
                "title": {"romaji": "Sousou no Frieren 3rd Season"},
                "startDate": {"year": 2027, "month": 10, "day": null}
            }
        ]))
        .unwrap();

        let snapshot = anime_snapshot(&seasons).unwrap();

        assert_eq!(snapshot.releases.len(), 2);
        assert!(snapshot.releases.contains_key("season:182255"));
        assert_eq!(
            snapshot.releases["season:209939"].date,
            Some(ReleaseDate::Partial {
                year: Some(2027),
                month: Some(10),
                day: None,
            })
        );
    }

    #[test]
    fn snapshot_contains_episode_metadata() {
        let mut anime = parse_anime(
            r#"{
        "data": {
            "Media": {
                "id": 154587,
                "title": {"english": "Frieren"},
                "coverImage": {
                    "medium": "https://example.com/cover.jpg"
                }
            }
        }
    }"#,
        )
        .unwrap();

        let airing_at = "2027-10-08T14:00:00Z".parse().unwrap();

        anime.scheduled_episodes.push(ScheduledEpisode {
            id: 1001,
            episode: 7,
            airing_at,
        });

        let snapshot = anime_snapshot(&[anime]).unwrap();
        let record = &snapshot.releases["episode:1001"];

        assert_eq!(snapshot.releases.len(), 2);
        assert_eq!(record.date, Some(ReleaseDate::Instant(airing_at)));
        assert_eq!(record.title.as_deref(), Some("Frieren — Episode 7"));
        assert_eq!(
            record.image_url.as_deref(),
            Some("https://example.com/cover.jpg")
        );
        assert_eq!(
            record.url.as_deref(),
            Some("https://anilist.co/anime/154587")
        );
    }
}
