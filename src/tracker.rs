use crate::anilist::AniListClient;
use crate::bubble::BubbleClient;
use crate::config;
use crate::notifier::{self, send_all};
use crate::presentation::{anime_updates, episode_reminders, show_upcoming};
use crate::state;
use anyhow::{Context, Result};

fn update_subscription(
    subscription: &config::Subscription,
    snapshot: state::SeriesSnapshot,
    state: &mut state::TrackerState,
    today: chrono::NaiveDate,
    messages: &mut Vec<String>,
) {
    match state.update_series(&subscription.provider, &subscription.id, snapshot) {
        Some(changes) => {
            for change in changes {
                if change.should_notify(today) {
                    println!("  Relevant change: {change:?}");
                    messages.push(format!("{}: {change:?}", subscription.name));
                }
            }
        }
        None => println!("  First run for this subscription: saving baseline."),
    }
}

pub(crate) fn process_bubble_subscription(
    subscription: &config::Subscription,
    bubble_client: &BubbleClient,
    state: &mut state::TrackerState,
    today: chrono::NaiveDate,
    messages: &mut Vec<String>,
) -> Result<()> {
    println!(
        "=== {}: {} ({}) ===",
        subscription.provider, subscription.name, subscription.id
    );

    let series = bubble_client
        .fetch_bubble_series(&subscription.id)
        .with_context(|| {
            format!(
                "Failed to fetch Bubble subscription '{}' ({})",
                subscription.name, subscription.id
            )
        })?;

    update_subscription(subscription, series.snapshot(), state, today, messages);

    show_upcoming(&series, &subscription.name, today, messages);

    Ok(())
}

pub(crate) fn process_anilist_subscription(
    subscription: &config::Subscription,
    client: &AniListClient,
    state: &mut state::TrackerState,
    today: chrono::NaiveDate,
    messages: &mut Vec<String>,
) -> Result<()> {
    let id = subscription.id.parse::<i32>().with_context(|| {
        format!(
            "Invalid AniList ID '{}' for subscription '{}'",
            subscription.id, subscription.name
        )
    })?;

    if id <= 0 {
        anyhow::bail!(
            "AniList ID must be positive for subscription '{}' ({id})",
            subscription.name
        );
    }

    let seasons = client.fetch_sequel_chain(id).with_context(|| {
        format!(
            "Failed to fetch AniList subscription '{}' ({id})",
            subscription.name
        )
    })?;

    println!("=== anilist: {} ({id}) ===", subscription.name);

    let snapshot = crate::anilist::anime_snapshot(&seasons).with_context(|| {
        format!(
            "Failed to build release snapshot for AniList subscription '{}' ({id})",
            subscription.name
        )
    })?;

    let reminders = episode_reminders(&snapshot, today);

    update_subscription(subscription, snapshot, state, today, messages);

    for message in reminders {
        println!("    Reminder: {message}");
        messages.push(message);
    }

    let updates = anime_updates(&seasons);
    if updates.is_empty() {
        println!("  No scheduled episodes or announced sequels.");
    } else {
        for update in updates {
            println!("  {update}");
        }
    }

    Ok(())
}

pub(crate) fn notify_and_save(
    notifier: &dyn notifier::Notifier,
    messages: &[String],
    state: &state::TrackerState,
    path: &std::path::Path,
) -> Result<()> {
    if let Err(failures) = send_all(notifier, messages) {
        for (index, error) in &failures {
            eprintln!("Notification {} failed: {error:#}", index + 1);
        }

        let count = failures.len();
        let (_, first_error) = failures.into_iter().next().expect("failures is nonempty");
        return Err(first_error.context(format!(
            "Notification delivery failed for {count} message(s); state was not saved"
        )));
    }

    state.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct FakeNotifier {
        fail: bool,
    }

    impl notifier::Notifier for FakeNotifier {
        fn send(&self, _message: &str) -> Result<()> {
            if self.fail {
                anyhow::bail!("delivery failed");
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn anilist_http_error_retains_subscription_request_and_status() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .expect(1)
            .mount(&server)
            .await;

        let endpoint = server.uri();
        let error = tokio::task::spawn_blocking(move || {
            let client = AniListClient::new(reqwest::blocking::Client::new(), endpoint);
            let subscription = config::Subscription {
                provider: "anilist".to_owned(),
                id: "123".to_owned(),
                name: "Test anime".to_owned(),
            };
            let mut state = state::TrackerState::default();
            let mut messages = Vec::new();
            let error = process_anilist_subscription(
                &subscription,
                &client,
                &mut state,
                chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
                &mut messages,
            )
            .unwrap_err();

            assert!(state.providers.is_empty());
            assert!(messages.is_empty());
            error
        })
        .await
        .unwrap();

        let chain = format!("{error:#}");
        assert!(chain.contains("AniList subscription 'Test anime' (123)"));
        assert!(chain.contains("season 123 in sequel chain rooted at 123"));
        assert!(chain.contains("anime 123, airing schedule page 1"));
        assert!(chain.contains("HTTP error"));
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(reqwest::StatusCode::TOO_MANY_REQUESTS)
        );
        server.verify().await;
    }

    #[test]
    fn saves_state_only_after_successful_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let previous = state::TrackerState::default();
        previous.save(&path).unwrap();

        let mut updated = state::TrackerState::default();
        updated
            .providers
            .insert("bubble".to_owned(), BTreeMap::new());
        let messages = vec!["New release".to_owned()];

        let failing = FakeNotifier { fail: true };
        let error = notify_and_save(&failing, &messages, &updated, &path).unwrap_err();
        let chain = format!("{error:#}");
        assert!(chain.contains("state was not saved"));
        assert!(chain.contains("Failed to send notification 1"));
        assert!(chain.contains("delivery failed"));
        assert_eq!(state::TrackerState::load(&path).unwrap(), previous);

        let successful = FakeNotifier { fail: false };
        notify_and_save(&successful, &messages, &updated, &path).unwrap();
        assert_eq!(state::TrackerState::load(&path).unwrap(), updated);
    }

    #[test]
    fn episode_reminders_select_today_and_one_week_only() {
        use crate::release_date::ReleaseDate;
        use crate::state::{ReleaseRecord, SeriesSnapshot};
        use chrono::{Duration, NaiveDate, TimeZone};
        use std::collections::BTreeMap;

        let today = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let mut releases = BTreeMap::new();

        for (episode, offset) in [(1, -1), (2, 0), (3, 6), (4, 7), (5, 8)] {
            let date = today + Duration::days(offset);

            // Local noon keeps the fixture independent of the machine's UTC offset.
            let instant = chrono::Local
                .from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
                .single()
                .unwrap()
                .with_timezone(&chrono::Utc);

            releases.insert(
                format!("episode:{episode}"),
                ReleaseRecord {
                    date: Some(ReleaseDate::Instant(instant)),
                    title: Some(format!("Example — Episode {episode}")),
                    url: Some("https://anilist.co/anime/123".to_owned()),
                    ..Default::default()
                },
            );
        }

        releases.insert(
            "season:123".to_owned(),
            ReleaseRecord {
                date: Some(ReleaseDate::Date(today)),
                ..Default::default()
            },
        );

        let snapshot = SeriesSnapshot { releases };

        assert_eq!(
            episode_reminders(&snapshot, today),
            vec![
                "Example — Episode 2 airs today (2026-11-08)\nhttps://anilist.co/anime/123",
                "Example — Episode 4 airs in one week (2026-11-15)\nhttps://anilist.co/anime/123",
            ]
        );
    }

    #[tokio::test]
    async fn queues_episode_reminders_on_baseline_and_unchanged_runs() {
        use chrono::{NaiveDate, TimeZone};
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let today = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let airing_at = chrono::Local
            .from_local_datetime(&today.and_hms_opt(12, 0, 0).unwrap())
            .single()
            .unwrap()
            .timestamp();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "Media": {
                        "id": 123,
                        "title": {"english": "Example"},
                        "airingSchedule": {
                            "pageInfo": {"hasNextPage": false},
                            "nodes": [{
                                "id": 456,
                                "episode": 7,
                                "airingAt": airing_at
                            }]
                        }
                    }
                }
            })))
            .expect(2)
            .mount(&server)
            .await;

        let endpoint = server.uri();
        tokio::task::spawn_blocking(move || {
            let client = AniListClient::new(reqwest::blocking::Client::new(), endpoint);
            let subscription = config::Subscription {
                provider: "anilist".to_owned(),
                id: "123".to_owned(),
                name: "Example".to_owned(),
            };
            let mut state = state::TrackerState::default();

            for _ in 0..2 {
                let mut messages = Vec::new();

                process_anilist_subscription(
                    &subscription,
                    &client,
                    &mut state,
                    today,
                    &mut messages,
                )
                .unwrap();

                assert_eq!(
                    messages,
                    vec![
                        "Example — Episode 7 airs today (2026-11-08)\nhttps://anilist.co/anime/123"
                    ]
                );
            }
        })
        .await
        .unwrap();

        server.verify().await;
    }
}
