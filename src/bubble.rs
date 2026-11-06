use chrono::{DateTime, Utc};
use serde::Deserialize;

pub fn fetch_bubble_series(id: &str) -> Result<BubbleSeries, Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let url = format!("https://api.bubblebd.com/v1.6/series/{id}");

    let response = client.get(url).send()?.error_for_status()?.text()?;

    Ok(parse_bubble_series(&response)?)
}

fn parse_bubble_series(contents: &str) -> Result<BubbleSeries, serde_json::Error> {
    serde_json::from_str(contents)
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
    title: Option<String>,
    tome: Option<u32>,
    prints: Vec<BubblePrint>,
}

#[derive(Debug, Deserialize)]
pub struct BubblePrint {
    #[serde(rename = "publicationDate")]
    publication_date: Option<DateTime<Utc>>,
}

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
