use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};

const ANIME_QUERY: &str = r#"
query ($id: Int!) {
    Media(id: $id, type: ANIME) {
        id
        title { romaji english }
        nextAiringEpisode { episode airingAt }
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

        let response = self
            .http
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()?
            .error_for_status()?
            .text()?;

        parse_anime(&response)
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
    let response: AniListResponse = serde_json::from_str(contents)?;

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

#[derive(Debug, Deserialize)]
struct AniListResponse {
    data: Option<AniListData>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct AniListData {
    #[serde(rename = "Media")]
    media: Option<Anime>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Anime {
    id: u32,
    title: AnimeTitle,
    #[serde(rename = "nextAiringEpisode")]
    pub(crate) next_airing_episode: Option<AiringEpisode>,
    relations: Option<AnimeRelations>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
