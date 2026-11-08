use super::model::{AiringSchedule, Anime, ScheduleMedia, ScheduledEpisode};
use crate::request_pacing::RequestGate;
use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
use std::time::Duration;

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
        tracing::debug!(anime = id, page = 1, "requesting AniList anime");

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
                    let wait = Duration::from_secs(retry_after.unwrap_or(60));
                    tracing::warn!(
                        pause = ?wait,
                        "AniList rate limited (429); pausing all AniList requests, then retrying once"
                    );
                    // Pausing the shared gate also holds back concurrent requests.
                    self.gate.pause(wait);
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
            tracing::debug!(anime = id, page, "requesting AniList schedule page");
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

pub(super) fn parse_media<T: serde::de::DeserializeOwned>(contents: &str) -> Result<T> {
    let response: AniListResponse<T> =
        serde_json::from_str(contents).context("Failed to parse AniList GraphQL response")?;

    if let Some(errors) = response.errors
        && !errors.is_empty()
    {
        let messages = errors
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>()
            .join("; ");

        anyhow::bail!("AniList GraphQL errors: {messages}");
    }

    let data = response.data.context("AniList response is missing data")?;

    let anime = data.media.context("AniList response contains no anime")?;

    Ok(anime)
}

#[derive(Debug, Deserialize)]
struct AniListResponse<T> {
    data: Option<AniListData<T>>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct AniListData<T> {
    #[serde(rename = "Media")]
    media: Option<T>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anilist::test_support::{FRIEREN_SEASON_2, TOO_MANY_REQUESTS, parse_anime};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test(start_paused = true)]
    async fn http_429_retries_once_then_explains_when_to_rerun() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(anime_request(123)))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_string(TOO_MANY_REQUESTS)
                    .insert_header("Retry-After", "30"),
            )
            .expect(2)
            .mount(&server)
            .await;

        let start = tokio::time::Instant::now();
        let error = client_for(&server).fetch_anime(123).await.unwrap_err();

        assert!(start.elapsed() >= Duration::from_secs(30));
        let chain = format!("{error:#}");
        assert!(chain.contains("Failed to fetch anime 123, airing schedule page 1"));
        assert!(chain.contains("AniList GraphQL request returned an HTTP error"));
        assert!(chain.contains("again after one automatic retry"));
        assert!(chain.contains("wait at least 30 seconds before rerunning"));
        assert_eq!(http_status(&error), Some(StatusCode::TOO_MANY_REQUESTS));
        server.verify().await;
    }

    #[tokio::test(start_paused = true)]
    async fn http_429_waits_for_retry_after_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "5"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ok(anime_page(123, false, json!([]))))
            .expect(1)
            .mount(&server)
            .await;

        let start = tokio::time::Instant::now();
        let anime = client_for(&server).fetch_anime(123).await.unwrap();

        assert_eq!(anime.id, 123);
        assert!(start.elapsed() >= Duration::from_secs(5));
        server.verify().await;
    }

    #[tokio::test(start_paused = true)]
    async fn http_429_with_missing_or_invalid_retry_after_gives_conservative_guidance() {
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

            let start = tokio::time::Instant::now();
            let error = client_for(&server).fetch_anime(123).await.unwrap_err();

            assert!(start.elapsed() >= Duration::from_secs(60));
            let chain = format!("{error:#}");
            assert!(chain.contains("Retry-After is missing or invalid"));
            assert!(chain.contains("Wait at least 60 seconds"));
            assert!(chain.contains("cooldown may be longer"));
            assert_eq!(http_status(&error), Some(StatusCode::TOO_MANY_REQUESTS));
            server.verify().await;
        }
    }

    #[tokio::test]
    async fn schedule_http_429_retains_page_context_and_returns_no_partial_anime() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(anime_request(123)))
            .respond_with(ok(anime_page(123, true, json!([episode(1, 1, 100)]))))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_json(schedule_request(123, 2)))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .expect(2)
            .mount(&server)
            .await;

        let error = client_for(&server).fetch_anime(123).await.unwrap_err();

        assert!(
            format!("{error:#}").contains("Failed to fetch airing schedule for anime 123, page 2")
        );
        assert_eq!(http_status(&error), Some(StatusCode::TOO_MANY_REQUESTS));
        server.verify().await;
    }

    #[tokio::test]
    async fn fetches_all_schedule_pages_and_deduplicates_events() {
        let server = MockServer::start().await;
        let shared = episode(2, 2, 200);
        Mock::given(method("POST"))
            .and(body_json(anime_request(1)))
            .respond_with(ok(anime_page(1, true, json!([shared.clone()]))))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_json(schedule_request(1, 2)))
            .respond_with(ok(schedule_page(
                false,
                json!([shared, episode(1, 1, 100)]),
            )))
            .expect(1)
            .mount(&server)
            .await;

        let anime = client_for(&server).fetch_anime(1).await.unwrap();

        let episode_ids: Vec<_> = anime
            .scheduled_episodes
            .iter()
            .map(|episode| episode.id)
            .collect();
        assert_eq!(episode_ids, [1, 2]);
        server.verify().await;
    }

    #[tokio::test]
    async fn schedule_page_failure_does_not_return_partial_anime() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(anime_request(1)))
            .respond_with(ok(anime_page(1, true, json!([episode(1, 1, 0)]))))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_json(schedule_request(1, 2)))
            .respond_with(ResponseTemplate::new(200).set_body_string(TOO_MANY_REQUESTS))
            .expect(1)
            .mount(&server)
            .await;

        let error = client_for(&server).fetch_anime(1).await.unwrap_err();

        let chain = format!("{error:#}");
        assert!(chain.contains("Failed to fetch airing schedule for anime 1, page 2"));
        assert!(chain.contains("AniList GraphQL errors: Too Many Requests."));
        server.verify().await;
    }

    #[tokio::test]
    async fn fetches_anime_using_graphql() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(anime_request(182255)))
            .respond_with(ResponseTemplate::new(200).set_body_string(FRIEREN_SEASON_2))
            .expect(1)
            .mount(&server)
            .await;

        let client = AniListClient::new(
            reqwest::Client::new(),
            format!("{}/graphql", server.uri()),
            Duration::ZERO,
        );
        let anime = client.fetch_anime(182255).await.unwrap();

        assert_eq!(anime.id, 182255);
        assert!(anime.next_airing_episode.is_none());
        let sequels = anime.announced_sequels();
        assert_eq!(sequels.len(), 1);
        assert_eq!(sequels[0].id, 209939);
        assert_eq!(sequels[0].title(), "Sousou no Frieren 3rd Season");
        assert_eq!(sequels[0].release_window(), "2027-10 (day unknown)");
        server.verify().await;
    }

    #[tokio::test]
    async fn follows_sequels_without_fetching_an_entry_twice() {
        let server = MockServer::start().await;
        // 1 → 2 → 3 → back to 1: a cycle must not cause a second fetch of 1.
        for (id, sequel_id) in [(1, 2), (2, 3), (3, 1)] {
            Mock::given(method("POST"))
                .and(body_json(anime_request(id)))
                .respond_with(ok(anime_with_sequel(id, sequel_id)))
                .expect(1)
                .mount(&server)
                .await;
        }

        let seasons = client_for(&server).fetch_sequel_chain(1).await.unwrap();

        let ids: Vec<u32> = seasons.iter().map(|anime| anime.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        server.verify().await;
    }

    #[test]
    fn reports_graphql_errors() {
        let error = parse_anime(TOO_MANY_REQUESTS).unwrap_err();

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

    // --- Builders for synthetic AniList requests and responses ---

    fn client_for(server: &MockServer) -> AniListClient {
        AniListClient::new(reqwest::Client::new(), server.uri(), Duration::ZERO)
    }

    fn ok(body: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(body)
    }

    fn http_status(error: &anyhow::Error) -> Option<StatusCode> {
        error.downcast_ref::<reqwest::Error>().unwrap().status()
    }

    fn anime_request(id: u32) -> Value {
        json!({ "query": ANIME_QUERY, "variables": { "id": id } })
    }

    fn schedule_request(id: u32, page: u32) -> Value {
        json!({ "query": SCHEDULE_QUERY, "variables": { "id": id, "page": page } })
    }

    fn episode(id: u32, number: u32, airing_at: i64) -> Value {
        json!({ "id": id, "episode": number, "airingAt": airing_at })
    }

    /// First response for an anime: its first schedule page and nothing else.
    fn anime_page(id: u32, has_next_page: bool, episodes: Value) -> Value {
        json!({ "data": { "Media": {
            "id": id,
            "title": {},
            "airingSchedule": { "pageInfo": { "hasNextPage": has_next_page }, "nodes": episodes },
        }}})
    }

    /// A later schedule page, as returned for `SCHEDULE_QUERY`.
    fn schedule_page(has_next_page: bool, episodes: Value) -> Value {
        json!({ "data": { "Media": {
            "airingSchedule": { "pageInfo": { "hasNextPage": has_next_page }, "nodes": episodes },
        }}})
    }

    fn anime_with_sequel(id: u32, sequel_id: u32) -> Value {
        let mut page = anime_page(id, false, json!([]));
        page["data"]["Media"]["relations"] = json!({ "edges": [{
            "relationType": "SEQUEL",
            "node": { "id": sequel_id, "type": "ANIME", "title": {}, "status": "FINISHED" },
        }]});
        page
    }
}
