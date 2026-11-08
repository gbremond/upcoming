use crate::release_date::ReleaseDate;
use crate::request_pacing::RequestGate;
use crate::state::{ReleaseRecord, SeriesSnapshot};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::StatusCode;
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
use std::time::Duration;

const ANIME_QUERY: &str = r#"
query ($id: Int!) {
    Media(id: $id, type: ANIME) {
        id
        title { romaji english }
        startDate { year month day }
        nextAiringEpisode { episode airingAt }
        coverImage { extraLarge }
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
    http: reqwest::Client,
    endpoint: String,
    gate: RequestGate,
}

impl AniListClient {
    pub(crate) fn new(http: reqwest::Client, endpoint: String, interval: Duration) -> Self {
        Self {
            http,
            endpoint,
            gate: RequestGate::new(interval),
        }
    }

    pub(crate) async fn fetch_anime(&self, id: i32) -> Result<Anime> {
        let body = serde_json::json!({
            "query": ANIME_QUERY,
            "variables": { "id": id }
        });

        let mut anime: Anime = self
            .request_media(body)
            .await
            .with_context(|| format!("Failed to fetch anime {id}, airing schedule page 1"))?;
        let first_page = anime.airing_schedule.take().ok_or_else(|| {
            anyhow::anyhow!("AniList response for anime {id} is missing the airing schedule")
        })?;
        anime.scheduled_episodes = self.fetch_remaining_schedule(id, first_page).await?;
        Ok(anime)
    }

    async fn request_media<T: serde::de::DeserializeOwned>(
        &self,
        body: serde_json::Value,
    ) -> Result<T> {
        let body = body.to_string();
        let mut attempt = 1;

        loop {
            self.gate.wait().await;

            let response = self
                .http
                .post(&self.endpoint)
                .header("Content-Type", "application/json")
                .body(body.clone())
                .send()
                .await
                .context("Failed to send AniList GraphQL request")?;

            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok());

                if attempt == 1 {
                    // Pausing the shared gate also holds back concurrent requests.
                    self.gate
                        .pause(Duration::from_secs(retry_after.unwrap_or(60)));
                    attempt += 1;
                    continue;
                }

                let guidance = match retry_after {
                    Some(seconds) => format!(
                        "AniList rate limit reached (429) again after one automatic retry; wait at least {seconds} seconds before rerunning"
                    ),
                    None => "AniList rate limit reached (429) again after one automatic retry; Retry-After is missing or invalid. Wait at least 60 seconds before trying again; the cooldown may be longer".to_owned(),
                };
                response
                    .error_for_status_ref()
                    .context("AniList GraphQL request returned an HTTP error")
                    .context(guidance)?;
            }

            let text = response
                .error_for_status()
                .context("AniList GraphQL request returned an HTTP error")?
                .text()
                .await
                .context("Failed to read AniList GraphQL response")?;

            return parse_media(&text);
        }
    }

    async fn fetch_remaining_schedule(
        &self,
        id: i32,
        mut schedule: AiringSchedule,
    ) -> Result<Vec<ScheduledEpisode>> {
        let mut episodes = std::collections::BTreeMap::new();
        let mut page = 1;

        loop {
            for episode in schedule.nodes {
                if let Some(previous) = episodes.get(&episode.id) {
                    if previous != &episode {
                        anyhow::bail!(
                            "AniList returned conflicting duplicate airing events for anime {id}, event {}, page {page}",
                            episode.id
                        );
                    }
                } else {
                    episodes.insert(episode.id, episode);
                }
            }

            if !schedule.page_info.has_next_page {
                break;
            }
            if page >= 100 {
                anyhow::bail!("AniList airing schedule for anime {id} exceeded 100 pages");
            }
            page += 1;
            let media: ScheduleMedia = self
                .request_media(serde_json::json!({
                    "query": SCHEDULE_QUERY,
                    "variables": { "id": id, "page": page }
                }))
                .await
                .with_context(|| {
                    format!("Failed to fetch airing schedule for anime {id}, page {page}")
                })?;
            schedule = media.airing_schedule;
        }

        let mut episodes: Vec<_> = episodes.into_values().collect();
        episodes.sort_by_key(|episode| (episode.airing_at, episode.id));
        Ok(episodes)
    }

    pub(crate) async fn fetch_sequel_chain(&self, root_id: i32) -> Result<Vec<Anime>> {
        if root_id <= 0 {
            anyhow::bail!("AniList ID must be positive");
        }

        let mut pending = VecDeque::from([root_id]);
        let mut visited = HashSet::new();
        let mut seasons = Vec::new();

        while let Some(id) = pending.pop_front() {
            if visited.contains(&id) {
                continue;
            }

            if visited.len() >= 20 {
                anyhow::bail!("AniList sequel traversal rooted at {root_id} exceeded 20 entries");
            }

            visited.insert(id);
            let anime = self.fetch_anime(id).await.with_context(|| {
                format!("Failed to fetch season {id} in sequel chain rooted at {root_id}")
            })?;

            for sequel in anime.sequels() {
                let sequel_id = i32::try_from(sequel.id).with_context(|| {
                    format!("Invalid sequel ID {} linked from anime {id}", sequel.id)
                })?;

                if !visited.contains(&sequel_id) {
                    pending.push_back(sequel_id);
                }
            }

            seasons.push(anime);
        }

        Ok(seasons)
    }
}

fn parse_media<T: serde::de::DeserializeOwned>(contents: &str) -> Result<T> {
    let response: AniListResponse<T> =
        serde_json::from_str(contents).context("Failed to parse AniList GraphQL response")?;

    if let Some(errors) = response.errors {
        if !errors.is_empty() {
            let messages = errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ");

            anyhow::bail!("AniList GraphQL errors: {messages}");
        }
    }

    let data = response.data.context("AniList response is missing data")?;

    let anime = data.media.context("AniList response contains no anime")?;

    Ok(anime)
}

pub(crate) fn anime_snapshot(seasons: &[Anime]) -> Result<SeriesSnapshot> {
    let mut releases = std::collections::BTreeMap::new();

    for season in seasons {
        releases.insert(
            format!("season:{}", season.id),
            season.release_record().with_context(|| {
                format!(
                    "Failed to map release record for anime {} ({})",
                    season.id,
                    season.title()
                )
            })?,
        );

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
    #[serde(rename = "extraLarge")]
    extra_large: Option<String>,
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

    fn release_record(&self) -> Result<ReleaseRecord> {
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
                .and_then(|cover| cover.extra_large.clone()),
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

    fn to_release_date(&self) -> Result<Option<ReleaseDate>> {
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
                .and_then(|cover| cover.extra_large.clone()),
            url: Some(format!("https://anilist.co/anime/{}", season.id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_anime(contents: &str) -> Result<Anime> {
        parse_media(contents)
    }

    #[tokio::test(start_paused = true)]
    async fn http_429_retries_once_then_explains_when_to_rerun() {
        use wiremock::matchers::{body_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let response = ResponseTemplate::new(429)
            .set_body_json(serde_json::json!({"errors": [{"message": "Too Many Requests."}]}))
            .insert_header("Retry-After", "30");
        Mock::given(method("POST"))
            .and(body_json(serde_json::json!({
                "query": ANIME_QUERY, "variables": {"id": 123}
            })))
            .respond_with(response)
            .expect(2)
            .mount(&server)
            .await;

        let endpoint = server.uri();
        let start = tokio::time::Instant::now();
        let error = {
            let client = AniListClient::new(reqwest::Client::new(), endpoint, Duration::ZERO);
            client.fetch_anime(123).await.unwrap_err()
        };

        assert!(start.elapsed() >= Duration::from_secs(30));
        let chain = format!("{error:#}");
        assert!(chain.contains("Failed to fetch anime 123, airing schedule page 1"));
        assert!(chain.contains("AniList GraphQL request returned an HTTP error"));
        assert!(chain.contains("again after one automatic retry"));
        assert!(chain.contains("wait at least 30 seconds before rerunning"));
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
        );
        server.verify().await;
    }

    #[tokio::test(start_paused = true)]
    async fn http_429_waits_for_retry_after_then_succeeds() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "5"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {"Media": {
                    "id": 123, "title": {},
                    "airingSchedule": {"pageInfo": {"hasNextPage": false}, "nodes": []}
                }}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = AniListClient::new(reqwest::Client::new(), server.uri(), Duration::ZERO);
        let start = tokio::time::Instant::now();
        let anime = client.fetch_anime(123).await.unwrap();

        assert_eq!(anime.id, 123);
        assert!(start.elapsed() >= Duration::from_secs(5));
        server.verify().await;
    }

    #[tokio::test(start_paused = true)]
    async fn http_429_with_missing_or_invalid_retry_after_gives_conservative_guidance() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for header in [None, Some("invalid")] {
            let server = MockServer::start().await;
            let mut response = ResponseTemplate::new(429);
            if let Some(value) = header {
                response = response.insert_header("Retry-After", value);
            }
            Mock::given(method("POST"))
                .respond_with(response)
                .expect(2)
                .mount(&server)
                .await;
            let endpoint = server.uri();
            let start = tokio::time::Instant::now();
            let error = {
                AniListClient::new(reqwest::Client::new(), endpoint, Duration::ZERO)
                    .fetch_anime(123)
                    .await
                    .unwrap_err()
            };

            assert!(start.elapsed() >= Duration::from_secs(60));
            let chain = format!("{error:#}");
            assert!(chain.contains("Retry-After is missing or invalid"));
            assert!(chain.contains("Wait at least 60 seconds"));
            assert!(chain.contains("cooldown may be longer"));
            assert_eq!(
                error.downcast_ref::<reqwest::Error>().unwrap().status(),
                Some(StatusCode::TOO_MANY_REQUESTS)
            );
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn schedule_http_429_retains_page_context_and_returns_no_partial_anime() {
        use wiremock::matchers::{body_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(serde_json::json!({
                "query": ANIME_QUERY, "variables": {"id": 123}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {"Media": {
                    "id": 123, "title": {},
                    "airingSchedule": {
                        "pageInfo": {"hasNextPage": true},
                        "nodes": [{"id": 1, "episode": 1, "airingAt": 100}]
                    }
                }}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_json(serde_json::json!({
                "query": SCHEDULE_QUERY, "variables": {"id": 123, "page": 2}
            })))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .expect(2)
            .mount(&server)
            .await;

        let endpoint = server.uri();
        let error = {
            let client = AniListClient::new(reqwest::Client::new(), endpoint, Duration::ZERO);
            client.fetch_anime(123).await.unwrap_err()
        };

        assert!(
            format!("{error:#}").contains("Failed to fetch airing schedule for anime 123, page 2")
        );
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
        );
        server.verify().await;
    }

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
        let anime = {
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap();
            AniListClient::new(http, endpoint, Duration::ZERO)
                .fetch_anime(1)
                .await
                .unwrap()
        };

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
        let error = {
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap();
            let error = AniListClient::new(http, endpoint, Duration::ZERO)
                .fetch_anime(1)
                .await
                .unwrap_err();
            format!("{error:#}")
        };
        assert!(error.contains("Failed to fetch airing schedule for anime 1, page 2"));
        assert!(error.contains("AniList GraphQL errors: Too Many Requests."));
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

        let anime = {
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap();

            let client = AniListClient::new(http, endpoint, Duration::ZERO);
            client.fetch_anime(182255).await.unwrap()
        };

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

        let seasons = {
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap();

            let client = AniListClient::new(http, endpoint, Duration::ZERO);
            client.fetch_sequel_chain(1).await.unwrap()
        };

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
                    "extraLarge": "https://example.com/cover.jpg"
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
