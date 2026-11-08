use crate::app::AppState;
use crate::config::Subscription;
use crate::release_date::ReleaseDate;
use crate::state::{ReleaseRecord, TrackerState};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{Datelike, NaiveDate};
use serde::Serialize;
use std::cmp::Reverse;

const RELEASE_LIMIT: usize = 3;

pub(crate) async fn get_subscriptions(
    State(app): State<AppState>,
) -> Result<Json<Vec<SubscriptionOverview>>, StatusCode> {
    let state = TrackerState::load(&app.state_path).map_err(|error| {
        tracing::error!("failed to load state: {error:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let today = chrono::Local::now().date_naive();
    Ok(Json(overviews(&app.subscriptions, &state, today)))
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct SubscriptionOverview {
    pub(crate) name: String,
    pub(crate) provider: String,
    /// Cover of the top release, or of the first announcement when nothing is dated.
    pub(crate) image_url: Option<String>,
    pub(crate) releases: Vec<Release>,
    pub(crate) announced: Vec<Announcement>,
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct Release {
    pub(crate) title: Option<String>,
    pub(crate) date: NaiveDate,
    pub(crate) upcoming: bool,
    pub(crate) image_url: Option<String>,
    pub(crate) url: Option<String>,
}

/// Confirmed but not yet scheduled to the day: an unaired season, or an album without a full date.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct Announcement {
    pub(crate) title: Option<String>,
    pub(crate) when: String,
    pub(crate) image_url: Option<String>,
    pub(crate) url: Option<String>,
}

/// One entry per subscription, ordered by next release (soonest first), then by name.
pub(crate) fn overviews(
    subscriptions: &[Subscription],
    state: &TrackerState,
    today: NaiveDate,
) -> Vec<SubscriptionOverview> {
    let mut entries: Vec<(Option<NaiveDate>, SubscriptionOverview)> = subscriptions
        .iter()
        .map(|subscription| {
            let SeriesRecords {
                seasons,
                dated,
                undated,
            } = SeriesRecords::of(subscription, state);
            let (next, releases) = select_releases(dated, today);
            let unaired_seasons = seasons.into_iter().filter(|record| {
                record
                    .date
                    .as_ref()
                    .and_then(ReleaseDate::reminder_date)
                    .is_none_or(|date| date >= today)
            });
            let announced = announcements(unaired_seasons.chain(undated));
            let image_url = releases
                .iter()
                .filter_map(|release| release.image_url.clone())
                .chain(announced.iter().filter_map(|a| a.image_url.clone()))
                .next();
            let overview = SubscriptionOverview {
                name: subscription.name.clone(),
                provider: subscription.provider.clone(),
                image_url,
                releases,
                announced,
            };
            (next, overview)
        })
        .collect();

    entries.sort_by(|(a_next, a), (b_next, b)| {
        (a_next.is_none(), a_next, &a.name).cmp(&(b_next.is_none(), b_next, &b.name))
    });
    entries.into_iter().map(|(_, overview)| overview).collect()
}

/// A subscription's stored records, split by how they are shown.
pub(crate) struct SeriesRecords<'a> {
    /// AniList season entries (`season:<id>` keys).
    pub(crate) seasons: Vec<&'a ReleaseRecord>,
    /// Episodes or albums with a full date.
    pub(crate) dated: Vec<(NaiveDate, &'a ReleaseRecord)>,
    /// Episodes or albums with a partial date or none.
    pub(crate) undated: Vec<&'a ReleaseRecord>,
}

impl<'a> SeriesRecords<'a> {
    pub(crate) fn of(subscription: &Subscription, state: &'a TrackerState) -> Self {
        let mut records = SeriesRecords {
            seasons: Vec::new(),
            dated: Vec::new(),
            undated: Vec::new(),
        };
        let Some(snapshot) = state
            .providers
            .get(&subscription.provider)
            .and_then(|series| series.get(&subscription.id))
        else {
            return records;
        };
        for (key, record) in &snapshot.releases {
            if key.starts_with("season:") {
                records.seasons.push(record);
            } else if let Some(date) = record.date.as_ref().and_then(ReleaseDate::reminder_date) {
                records.dated.push((date, record));
            } else {
                records.undated.push(record);
            }
        }
        records
    }
}

/// Closest upcoming releases first, then fill with the latest released, newest on top.
/// Also returns the next release date, used to order subscriptions.
fn select_releases(
    dated: Vec<(NaiveDate, &ReleaseRecord)>,
    today: NaiveDate,
) -> (Option<NaiveDate>, Vec<Release>) {
    let (mut upcoming, mut latest): (Vec<_>, Vec<_>) =
        dated.into_iter().partition(|(date, _)| *date >= today);

    upcoming.sort_by_key(|(date, _)| *date);
    upcoming.truncate(RELEASE_LIMIT);
    let next = upcoming.first().map(|(date, _)| *date);

    latest.sort_by_key(|(date, _)| Reverse(*date));
    latest.truncate(RELEASE_LIMIT - upcoming.len());

    let releases = upcoming
        .into_iter()
        .rev()
        .map(|(date, record)| release(date, record, true))
        .chain(
            latest
                .into_iter()
                .map(|(date, record)| release(date, record, false)),
        )
        .collect();
    (next, releases)
}

pub(crate) fn release(date: NaiveDate, record: &ReleaseRecord, upcoming: bool) -> Release {
    Release {
        title: record.title.clone(),
        date,
        upcoming,
        image_url: record.image_url.clone(),
        url: record.url.clone(),
    }
}

/// Soonest first; the less precise the date, the later it sorts, and unknown dates come last.
fn announcements<'a>(records: impl Iterator<Item = &'a ReleaseRecord>) -> Vec<Announcement> {
    let mut records: Vec<&ReleaseRecord> = records.collect();
    records.sort_by_key(|record| announcement_order(record.date.as_ref()));
    records
        .into_iter()
        .map(|record| Announcement {
            title: record.title.clone(),
            when: describe_when(record.date.as_ref()),
            image_url: record.image_url.clone(),
            url: record.url.clone(),
        })
        .collect()
}

fn announcement_order(date: Option<&ReleaseDate>) -> (i32, u32, u32) {
    match date {
        Some(ReleaseDate::Partial { year, month, day }) => (
            year.unwrap_or(i32::MAX),
            month.unwrap_or(u32::MAX),
            day.unwrap_or(u32::MAX),
        ),
        Some(date) => match date.reminder_date() {
            Some(date) => (date.year(), date.month(), date.day()),
            None => (i32::MAX, u32::MAX, u32::MAX),
        },
        None => (i32::MAX, u32::MAX, u32::MAX),
    }
}

fn describe_when(date: Option<&ReleaseDate>) -> String {
    let full = |date: NaiveDate| date.format("%d %b %Y").to_string();
    match date {
        Some(ReleaseDate::Partial {
            year: Some(year),
            month,
            day,
        }) => match (month, day) {
            (Some(month), Some(day)) => NaiveDate::from_ymd_opt(*year, *month, *day)
                .map(full)
                .unwrap_or_else(|| year.to_string()),
            (Some(month), None) => NaiveDate::from_ymd_opt(*year, *month, 1)
                .map(|date| date.format("%b %Y").to_string())
                .unwrap_or_else(|| year.to_string()),
            (None, _) => year.to_string(),
        },
        Some(date) => date
            .reminder_date()
            .map(full)
            .unwrap_or_else(|| "TBA".to_owned()),
        None => "TBA".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SeriesSnapshot;
    use axum::body::Body;
    use axum::http::Request;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn today() -> NaiveDate {
        day(2026, 11, 8)
    }

    fn subscription(provider: &str, id: &str, name: &str) -> Subscription {
        Subscription {
            provider: provider.to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
        }
    }

    fn record(title: &str, date: Option<ReleaseDate>) -> ReleaseRecord {
        ReleaseRecord {
            date,
            title: Some(title.to_owned()),
            ..ReleaseRecord::default()
        }
    }

    fn on(date: NaiveDate) -> Option<ReleaseDate> {
        Some(ReleaseDate::Date(date))
    }

    fn state_with(entries: &[(&str, &str, Vec<ReleaseRecord>)]) -> TrackerState {
        let mut state = TrackerState::default();
        for (provider, id, records) in entries {
            let releases = records
                .iter()
                .enumerate()
                .map(|(index, record)| (format!("release-{index}"), record.clone()))
                .collect();
            state
                .providers
                .entry((*provider).to_owned())
                .or_default()
                .insert((*id).to_owned(), SeriesSnapshot { releases });
        }
        state
    }

    /// (title, upcoming) of the single subscription's releases.
    fn titles(subscriptions: &[Subscription], state: &TrackerState) -> Vec<(String, bool)> {
        let overviews = overviews(subscriptions, state, today());
        assert_eq!(overviews.len(), 1);
        overviews[0]
            .releases
            .iter()
            .map(|release| (release.title.clone().unwrap(), release.upcoming))
            .collect()
    }

    fn owned(expected: &[(&str, bool)]) -> Vec<(String, bool)> {
        expected
            .iter()
            .map(|(title, upcoming)| ((*title).to_owned(), *upcoming))
            .collect()
    }

    #[test]
    fn without_upcoming_shows_the_three_latest_however_old() {
        let subscriptions = [subscription("bubble", "5t", "Les 5 Terres")];
        let state = state_with(&[(
            "bubble",
            "5t",
            vec![
                record("T14", on(day(2020, 3, 1))),
                record("T17", on(day(2026, 5, 21))),
                record("T15", on(day(2021, 1, 6))),
                record("T16", on(day(2024, 9, 1))),
            ],
        )]);

        assert_eq!(
            titles(&subscriptions, &state),
            owned(&[("T17", false), ("T16", false), ("T15", false)])
        );
    }

    #[test]
    fn upcoming_releases_come_first_and_latest_fill_the_rest() {
        let subscriptions = [subscription("bubble", "5t", "Les 5 Terres")];
        let state = state_with(&[(
            "bubble",
            "5t",
            vec![
                record("T16", on(day(2024, 9, 1))),
                record("T19", on(day(2027, 6, 1))),
                record("T17", on(day(2026, 5, 21))),
                record("T18", on(day(2027, 1, 15))),
            ],
        )]);

        assert_eq!(
            titles(&subscriptions, &state),
            owned(&[("T19", true), ("T18", true), ("T17", false)])
        );
    }

    #[test]
    fn many_upcoming_keeps_only_the_three_closest() {
        let subscriptions = [subscription("anilist", "1", "Frieren")];
        let state = state_with(&[(
            "anilist",
            "1",
            vec![
                record("Ep 11", on(day(2026, 11, 1))),
                record("Ep 15", on(day(2026, 12, 6))),
                record("Ep 12", on(day(2026, 11, 13))),
                record("Ep 14", on(day(2026, 11, 27))),
                record("Ep 13", on(day(2026, 11, 20))),
            ],
        )]);

        assert_eq!(
            titles(&subscriptions, &state),
            owned(&[("Ep 14", true), ("Ep 13", true), ("Ep 12", true)])
        );
    }

    #[test]
    fn a_release_today_is_upcoming() {
        let subscriptions = [subscription("anilist", "1", "Frieren")];
        let state = state_with(&[(
            "anilist",
            "1",
            vec![
                record("Today", on(today())),
                record("Yesterday", on(today().pred_opt().unwrap())),
            ],
        )]);

        assert_eq!(
            titles(&subscriptions, &state),
            owned(&[("Today", true), ("Yesterday", false)])
        );
    }

    #[test]
    fn subscriptions_are_ordered_by_next_release_then_name() {
        let subscriptions = [
            subscription("bubble", "z", "Zebra"),
            subscription("bubble", "later", "Later"),
            subscription("bubble", "a", "Alpha"),
            subscription("bubble", "soon", "Soon"),
        ];
        let state = state_with(&[
            ("bubble", "later", vec![record("L", on(day(2027, 1, 1)))]),
            ("bubble", "soon", vec![record("S", on(day(2026, 12, 1)))]),
            ("bubble", "z", vec![record("Z", on(day(2020, 1, 1)))]),
        ]);

        let names: Vec<String> = overviews(&subscriptions, &state, today())
            .into_iter()
            .map(|overview| overview.name)
            .collect();

        assert_eq!(names, ["Soon", "Later", "Alpha", "Zebra"]);
    }

    #[test]
    fn a_subscription_not_fetched_yet_is_listed_without_releases() {
        let subscriptions = [subscription("anilist", "42", "New show")];

        assert_eq!(
            overviews(&subscriptions, &TrackerState::default(), today()),
            vec![SubscriptionOverview {
                name: "New show".to_owned(),
                provider: "anilist".to_owned(),
                image_url: None,
                releases: vec![],
                announced: vec![],
            }]
        );
    }

    fn keyed(entries: &[(&str, ReleaseRecord)]) -> TrackerState {
        let releases = entries
            .iter()
            .map(|(key, record)| ((*key).to_owned(), record.clone()))
            .collect();
        let mut state = TrackerState::default();
        state
            .providers
            .entry("anilist".to_owned())
            .or_default()
            .insert("1".to_owned(), SeriesSnapshot { releases });
        state
    }

    fn partial(year: Option<i32>, month: Option<u32>) -> Option<ReleaseDate> {
        Some(ReleaseDate::Partial {
            year,
            month,
            day: None,
        })
    }

    #[test]
    fn seasons_are_announced_not_listed_as_releases() {
        let subscriptions = [subscription("anilist", "1", "Frieren")];
        let state = keyed(&[
            ("season:1", record("Season 1", on(day(2023, 9, 29)))),
            (
                "season:3",
                record("Season 3", partial(Some(2027), Some(10))),
            ),
            ("season:4", record("Season 4", None)),
            ("season:2", record("Season 2", on(day(2027, 1, 15)))),
            ("episode:10", record("Ep 10", on(day(2026, 3, 27)))),
        ]);

        let overview = &overviews(&subscriptions, &state, today())[0];

        let releases: Vec<_> = overview
            .releases
            .iter()
            .map(|r| r.title.as_deref())
            .collect();
        assert_eq!(releases, [Some("Ep 10")]);
        let announced: Vec<_> = overview
            .announced
            .iter()
            .map(|a| (a.title.as_deref().unwrap(), a.when.as_str()))
            .collect();
        assert_eq!(
            announced,
            [
                ("Season 2", "15 Jan 2027"),
                ("Season 3", "Oct 2027"),
                ("Season 4", "TBA"),
            ]
        );
    }

    #[test]
    fn albums_without_a_full_date_are_announced() {
        let subscriptions = [subscription("bubble", "5t", "Les 5 Terres")];
        let state = state_with(&[(
            "bubble",
            "5t",
            vec![
                record("T19", None),
                record("T18", partial(Some(2027), None)),
                record("T17", on(day(2026, 5, 21))),
            ],
        )]);

        let overview = &overviews(&subscriptions, &state, today())[0];

        assert_eq!(overview.releases.len(), 1);
        let announced: Vec<_> = overview
            .announced
            .iter()
            .map(|a| (a.title.as_deref().unwrap(), a.when.as_str()))
            .collect();
        assert_eq!(announced, [("T18", "2027"), ("T19", "TBA")]);
    }

    #[test]
    fn image_comes_from_the_top_release_then_announcements() {
        let subscriptions = [subscription("anilist", "1", "Frieren")];
        let cover = |title: &str, date, image: Option<&str>| ReleaseRecord {
            image_url: image.map(str::to_owned),
            ..record(title, date)
        };

        let dated = keyed(&[
            (
                "episode:1",
                cover("Ep 1", on(day(2026, 1, 1)), Some("old.jpg")),
            ),
            ("episode:2", cover("Ep 2", on(day(2026, 12, 1)), None)),
            ("season:3", cover("S3", None, Some("s3.jpg"))),
        ]);
        let only_announced = keyed(&[("season:3", cover("S3", None, Some("s3.jpg")))]);

        let image = |state| {
            overviews(&subscriptions, &state, today())
                .remove(0)
                .image_url
        };
        assert_eq!(image(dated).as_deref(), Some("old.jpg"));
        assert_eq!(image(only_announced).as_deref(), Some("s3.jpg"));
    }

    #[tokio::test]
    async fn serves_overviews_from_the_state_file() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        state_with(&[(
            "bubble",
            "5t",
            vec![
                record("T17", on(day(2000, 1, 1))),
                record("T18", on(day(2999, 1, 1))),
                record("T19", None),
            ],
        )])
        .save(&state_path)
        .unwrap();
        let app = crate::app::router(AppState {
            status: Default::default(),
            subscriptions: Arc::new(vec![subscription("bubble", "5t", "Les 5 Terres")]),
            state_path,
        });

        let response = app
            .oneshot(Request::get("/subscriptions").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{
                "name": "Les 5 Terres",
                "provider": "bubble",
                "image_url": null,
                "releases": [
                    { "title": "T18", "date": "2999-01-01", "upcoming": true, "image_url": null, "url": null },
                    { "title": "T17", "date": "2000-01-01", "upcoming": false, "image_url": null, "url": null },
                ],
                "announced": [
                    { "title": "T19", "when": "TBA", "image_url": null, "url": null },
                ],
            }])
        );
    }

    #[tokio::test]
    async fn a_corrupt_state_file_is_a_server_error() {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        std::fs::write(&state_path, "not json").unwrap();
        let app = crate::app::router(AppState {
            status: Default::default(),
            subscriptions: Arc::new(vec![]),
            state_path,
        });

        let response = app
            .oneshot(Request::get("/subscriptions").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
