use crate::release_date::ReleaseDate;
use crate::state::{ReleaseRecord, SeriesSnapshot};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

impl BubbleClient {
    pub fn new(http_client: reqwest::blocking::Client, base_url: String) -> Self {
        Self {
            http_client,
            base_url,
        }
    }

    pub fn fetch_bubble_series(
        &self,
        id: &str,
    ) -> Result<BubbleSeries, Box<dyn std::error::Error>> {
        let url = format!("{}/series/{id}", self.base_url.trim_end_matches('/'));

        let response = self
            .http_client
            .get(url)
            .send()?
            .error_for_status()?
            .text()?;

        Ok(parse_bubble_series(&response)?)
    }
}

fn parse_bubble_series(contents: &str) -> Result<BubbleSeries, serde_json::Error> {
    serde_json::from_str(contents)
}

pub struct BubbleClient {
    http_client: reqwest::blocking::Client,
    base_url: String,
}

#[derive(Debug, Deserialize)]
pub struct BubbleSeries {
    title: String,
    albums: Vec<BubbleAlbum>,
}

#[derive(Debug, Deserialize)]
pub struct BubbleAlbum {
    #[serde(rename = "objectId")]
    id: String,
    pub title: Option<String>,
    pub tome: Option<u32>,
    prints: Vec<BubblePrint>,
    images: Option<AlbumImages>,
    permalink: Option<String>,
    #[serde(rename = "defaultSellingPrintObjectId")]
    default_selling_print_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AlbumImages {
    front: Option<CoverImage>,
}

#[derive(Debug, Deserialize)]
struct CoverImage {
    medium: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BubblePrint {
    #[serde(rename = "publicationDate")]
    publication_date: Option<DateTime<Utc>>,
}

impl BubbleSeries {
    pub fn upcoming_albums(&self, today: NaiveDate) -> Vec<(&BubbleAlbum, NaiveDate)> {
        let mut upcoming = Vec::new();

        for album in &self.albums {
            if let Some(date) = album.first_publication_date() {
                if date >= today {
                    upcoming.push((album, date));
                }
            }
        }

        upcoming.sort_by_key(|(_, date)| *date);
        upcoming
    }

    pub(crate) fn snapshot(&self) -> SeriesSnapshot {
        let releases = self
            .albums
            .iter()
            .map(|album| {
                (
                    album.id.clone(),
                    ReleaseRecord {
                        date: album.first_publication_date().map(ReleaseDate::Date),
                        title: album.title.clone(),
                        image_url: album.cover_url().map(str::to_owned),
                        url: album.page_url(),
                    },
                )
            })
            .collect();

        SeriesSnapshot { releases }
    }
}

impl BubbleAlbum {
    fn first_publication_date(&self) -> Option<NaiveDate> {
        self.prints
            .iter()
            .filter_map(|print| {
                print
                    .publication_date
                    .as_ref()
                    .map(|date| date.date_naive())
            })
            .min()
    }

    fn cover_url(&self) -> Option<&str> {
        self.images.as_ref()?.front.as_ref()?.medium.as_deref()
    }

    fn page_url(&self) -> Option<String> {
        let permalink = self.permalink.as_deref()?;
        let print_id = self.default_selling_print_id.as_deref()?;

        Some(format!(
            "https://www.bubblebd.com/{permalink}/album/{}/{print_id}",
            self.id
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_bubble_series_with_null_fields() {
        let json = r#"{
        "title": "Les 5 Terres",
        "albums": [{
            "objectId": "album-18",
            "title": null,
            "tome": 18,
            "prints": [
                {"publicationDate": "2026-10-08T00:00:00.000Z"},
                {"publicationDate": null}
            ]
        }]
    }"#;

        let series = parse_bubble_series(json).unwrap();

        assert_eq!(series.title, "Les 5 Terres");
        assert_eq!(series.albums.len(), 1);

        let album = &series.albums[0];
        assert_eq!(album.id, "album-18");
        assert_eq!(album.title, None);
        assert_eq!(album.tome, Some(18));
        let expected = "2026-10-08T00:00:00.000Z".parse::<DateTime<Utc>>().unwrap();

        assert_eq!(album.prints[0].publication_date, Some(expected));
        assert_eq!(album.prints[1].publication_date, None);
    }

    #[test]
    fn upcoming_albums_excludes_reprints_and_unknown_dates() {
        let json = r#"{
        "title": "Example",
        "albums": [
            {
                "objectId": "future",
                "prints": [{"publicationDate": "2026-12-10T00:00:00Z"}]
            },
            {
                "objectId": "reprint",
                "prints": [
                    {"publicationDate": "2026-12-10T00:00:00Z"},
                    {"publicationDate": "2020-01-01T00:00:00Z"}
                ]
            },
            {
                "objectId": "today",
                "prints": [{"publicationDate": "2026-11-06T00:00:00Z"}]
            },
            {
                "objectId": "unknown",
                "prints": [{"publicationDate": null}]
            }
        ]
    }"#;

        let series = parse_bubble_series(json).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();

        let upcoming = series.upcoming_albums(today);

        let ids: Vec<&str> = upcoming
            .iter()
            .map(|(album, _)| album.id.as_str())
            .collect();

        assert_eq!(ids, vec!["today", "future"]);
        assert_eq!(upcoming[0].1, today);
    }

    #[tokio::test]
    async fn fetches_bubble_series() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1.6/series/example-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "Example series",
                "albums": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let base_url = format!("{}/v1.6/", server.uri());

        let series = tokio::task::spawn_blocking(move || {
            let client = test_client(base_url);

            client.fetch_bubble_series("example-id").unwrap()
        })
        .await
        .unwrap();

        assert_eq!(series.title, "Example series");
        assert!(series.albums.is_empty());

        server.verify().await;
    }

    #[tokio::test]
    async fn propagates_http_errors() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1.6/series/example-id"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;

        let base_url = format!("{}/v1.6", server.uri());

        let status = tokio::task::spawn_blocking(move || {
            let client = test_client(base_url);
            let error = client.fetch_bubble_series("example-id").unwrap_err();

            error
                .downcast_ref::<reqwest::Error>()
                .expect("expected a Reqwest error")
                .status()
        })
        .await
        .unwrap();

        assert_eq!(status, Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR));

        server.verify().await;
    }

    fn test_client(base_url: String) -> BubbleClient {
        let http_client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();

        BubbleClient::new(http_client, base_url)
    }
}
