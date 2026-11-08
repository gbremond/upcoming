use crate::app::AppState;
use crate::config::Subscription;
use crate::state::TrackerState;
use crate::subscriptions::{Release, SeriesRecords, release};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
pub(crate) struct ReleasesQuery {
    limit: Option<usize>,
    /// Caps how many releases a single series may take, so a weekly anime can't fill the list.
    per_series: Option<usize>,
}

pub(crate) async fn get_releases(
    State(app): State<AppState>,
    Query(query): Query<ReleasesQuery>,
) -> Result<Json<Vec<SeriesRelease>>, StatusCode> {
    let state = TrackerState::load(&app.state_path).map_err(|error| {
        tracing::error!("failed to load state: {error:#}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let today = chrono::Local::now().date_naive();
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let per_series = query.per_series.unwrap_or(limit);
    Ok(Json(closest_releases(
        &app.subscriptions,
        &state,
        today,
        limit,
        per_series,
    )))
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct SeriesRelease {
    pub(crate) series: String,
    pub(crate) provider: String,
    #[serde(flatten)]
    pub(crate) release: Release,
}

/// The `limit` dated releases nearest to today, past or future, in chronological order.
/// On equal distance, the upcoming one wins. Each series keeps at most `per_series` releases.
pub(crate) fn closest_releases(
    subscriptions: &[Subscription],
    state: &TrackerState,
    today: NaiveDate,
    limit: usize,
    per_series: usize,
) -> Vec<SeriesRelease> {
    let mut candidates: Vec<SeriesRelease> = subscriptions
        .iter()
        .flat_map(|subscription| {
            SeriesRecords::of(subscription, state)
                .dated
                .into_iter()
                .map(move |(date, record)| SeriesRelease {
                    series: subscription.name.clone(),
                    provider: subscription.provider.clone(),
                    release: release(date, record, date >= today),
                })
        })
        .collect();

    candidates.sort_by_key(|item| {
        let date = item.release.date;
        (
            (date - today).num_days().abs(),
            date < today,
            item.series.clone(),
        )
    });
    let mut taken: HashMap<String, usize> = HashMap::new();
    candidates.retain(|item| {
        let count = taken.entry(item.series.clone()).or_default();
        *count += 1;
        *count <= per_series
    });
    candidates.truncate(limit);
    candidates.sort_by(|a, b| {
        (a.release.date, &a.series, &a.release.title).cmp(&(
            b.release.date,
            &b.series,
            &b.release.title,
        ))
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release_date::ReleaseDate;
    use crate::state::{ReleaseRecord, SeriesSnapshot};
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

    fn subscription(id: &str, name: &str) -> Subscription {
        Subscription {
            provider: "bubble".to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
        }
    }

    /// (key, title, date) of one stored release.
    type Entry<'a> = (&'a str, &'a str, Option<NaiveDate>);

    fn state_with(entries: &[(&str, &[Entry])]) -> TrackerState {
        let mut state = TrackerState::default();
        for (id, records) in entries {
            let releases = records
                .iter()
                .map(|(key, title, date)| {
                    let record = ReleaseRecord {
                        date: date.map(ReleaseDate::Date),
                        title: Some((*title).to_owned()),
                        ..ReleaseRecord::default()
                    };
                    ((*key).to_owned(), record)
                })
                .collect();
            state
                .providers
                .entry("bubble".to_owned())
                .or_default()
                .insert((*id).to_owned(), SeriesSnapshot { releases });
        }
        state
    }

    fn summary(releases: &[SeriesRelease]) -> Vec<(&str, &str, bool)> {
        releases
            .iter()
            .map(|item| {
                (
                    item.series.as_str(),
                    item.release.title.as_deref().unwrap(),
                    item.release.upcoming,
                )
            })
            .collect()
    }

    #[test]
    fn keeps_the_closest_releases_in_chronological_order() {
        let subscriptions = [subscription("a", "Anime"), subscription("b", "BD")];
        let state = state_with(&[
            (
                "a",
                &[
                    ("e1", "Ep 1", Some(day(2026, 10, 1))),
                    ("e2", "Ep 2", Some(day(2026, 11, 6))),
                    ("e3", "Ep 3", Some(day(2026, 11, 13))),
                    ("e4", "Ep 4", Some(day(2026, 12, 25))),
                ],
            ),
            (
                "b",
                &[
                    ("t1", "T1", Some(day(2020, 1, 1))),
                    ("t2", "T2", Some(day(2026, 11, 1))),
                    ("t3", "T3", Some(day(2026, 11, 20))),
                ],
            ),
        ]);

        let releases = closest_releases(&subscriptions, &state, today(), 4, 4);

        assert_eq!(
            summary(&releases),
            [
                ("BD", "T2", false),
                ("Anime", "Ep 2", false),
                ("Anime", "Ep 3", true),
                ("BD", "T3", true),
            ]
        );
    }

    #[test]
    fn on_equal_distance_the_upcoming_release_wins() {
        let subscriptions = [subscription("a", "Anime")];
        let state = state_with(&[(
            "a",
            &[
                ("past", "Past", Some(day(2026, 11, 5))),
                ("future", "Future", Some(day(2026, 11, 11))),
            ],
        )]);

        let releases = closest_releases(&subscriptions, &state, today(), 1, 1);

        assert_eq!(summary(&releases), [("Anime", "Future", true)]);
    }

    #[test]
    fn seasons_and_undated_releases_are_ignored() {
        let subscriptions = [subscription("a", "Anime")];
        let state = state_with(&[(
            "a",
            &[
                ("season:1", "Season 1", Some(today())),
                ("e1", "Undated", None),
                ("e2", "Ep 2", Some(day(2024, 1, 1))),
            ],
        )]);

        let releases = closest_releases(&subscriptions, &state, today(), 5, 5);

        assert_eq!(summary(&releases), [("Anime", "Ep 2", false)]);
    }

    #[test]
    fn per_series_cap_makes_room_for_other_series() {
        let subscriptions = [subscription("a", "Anime"), subscription("b", "BD")];
        let state = state_with(&[
            (
                "a",
                &[
                    ("e1", "Ep 1", Some(day(2026, 11, 1))),
                    ("e2", "Ep 2", Some(day(2026, 11, 8))),
                    ("e3", "Ep 3", Some(day(2026, 11, 15))),
                ],
            ),
            ("b", &[("t1", "T1", Some(day(2027, 3, 1)))]),
        ]);

        let releases = closest_releases(&subscriptions, &state, today(), 3, 2);

        assert_eq!(
            summary(&releases),
            [
                ("Anime", "Ep 2", true),
                ("Anime", "Ep 3", true),
                ("BD", "T1", true),
            ]
        );
    }

    async fn get_json(uri: &str, state: &TrackerState) -> serde_json::Value {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        state.save(&state_path).unwrap();
        let app = crate::app::router(AppState {
            status: Default::default(),
            subscriptions: Arc::new(vec![subscription("a", "Anime")]),
            state_path,
        });

        let response = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn serves_flat_releases_with_the_series_name() {
        let state = state_with(&[("a", &[("e1", "Ep 1", Some(day(2000, 1, 1)))])]);

        let json = get_json("/releases", &state).await;

        assert_eq!(
            json,
            serde_json::json!([{
                "series": "Anime",
                "provider": "bubble",
                "title": "Ep 1",
                "date": "2000-01-01",
                "upcoming": false,
                "image_url": null,
                "url": null,
            }])
        );
    }

    #[tokio::test]
    async fn limit_query_parameter_caps_the_result() {
        let records: Vec<(String, String, Option<NaiveDate>)> = (1..=8)
            .map(|n| (format!("e{n}"), format!("Ep {n}"), Some(day(2000, 1, n))))
            .collect();
        let borrowed: Vec<Entry> = records
            .iter()
            .map(|(key, title, date)| (key.as_str(), title.as_str(), *date))
            .collect();
        let state = state_with(&[("a", &borrowed)]);

        assert_eq!(
            get_json("/releases", &state)
                .await
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            get_json("/releases?limit=4&per_series=2", &state)
                .await
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}
