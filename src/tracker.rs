use crate::anilist::{AniListClient, Anime};
use crate::bubble::{BubbleClient, BubbleSeries};
use crate::config;
use crate::notifier::{self, NotificationReason, ReleaseNotification, send_all};
use crate::presentation::{anime_updates, episode_reminders, show_upcoming};
use crate::state;
use anyhow::{Context, Result};

fn update_subscription(
    subscription: &config::Subscription,
    snapshot: state::SeriesSnapshot,
    state: &mut state::TrackerState,
    today: chrono::NaiveDate,
    messages: &mut Vec<ReleaseNotification>,
) {
    match state.update_series(&subscription.provider, &subscription.id, snapshot) {
        Some(changes) => {
            for change in changes {
                if !change.should_notify(today) {
                    continue;
                }

                let (id, reason) = match change {
                    state::ReleaseChange::Added { id, .. } => (id, NotificationReason::Added),
                    state::ReleaseChange::DateChanged { id, old_date, .. } => {
                        (id, NotificationReason::DateChanged { old_date })
                    }
                };

                let release =
                    state.providers[&subscription.provider][&subscription.id].releases[&id].clone();

                messages.push(ReleaseNotification {
                    subscription_name: subscription.name.clone(),
                    release,
                    reason,
                });
            }
        }
        None => println!("  First run for this subscription: saving baseline."),
    }
}

pub(crate) enum Fetched {
    Bubble(BubbleSeries),
    AniList(Vec<Anime>),
}

pub(crate) async fn fetch_subscription(
    subscription: &config::Subscription,
    bubble_client: &BubbleClient,
    anilist_client: &AniListClient,
) -> Result<Fetched> {
    let started = std::time::Instant::now();
    eprintln!(
        "→ Fetching {}: {} ({})",
        subscription.provider, subscription.name, subscription.id
    );

    let fetched = match subscription.provider.as_str() {
        "bubble" => {
            let series = bubble_client
                .fetch_bubble_series(&subscription.id)
                .await
                .with_context(|| {
                    format!(
                        "Failed to fetch Bubble subscription '{}' ({})",
                        subscription.name, subscription.id
                    )
                })?;
            Ok(Fetched::Bubble(series))
        }
        "anilist" => {
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

            let seasons = anilist_client
                .fetch_sequel_chain(id)
                .await
                .with_context(|| {
                    format!(
                        "Failed to fetch AniList subscription '{}' ({id})",
                        subscription.name
                    )
                })?;
            Ok(Fetched::AniList(seasons))
        }
        provider => anyhow::bail!(
            "Unsupported provider '{provider}' for subscription '{}'",
            subscription.name
        ),
    };

    if fetched.is_ok() {
        eprintln!(
            "✓ Fetched {}: {} in {:.1?}",
            subscription.provider,
            subscription.name,
            started.elapsed()
        );
    }

    fetched
}

pub(crate) fn apply_subscription(
    subscription: &config::Subscription,
    fetched: Fetched,
    state: &mut state::TrackerState,
    today: chrono::NaiveDate,
    messages: &mut Vec<ReleaseNotification>,
) -> Result<()> {
    println!(
        "=== {}: {} ({}) ===",
        subscription.provider, subscription.name, subscription.id
    );

    match fetched {
        Fetched::Bubble(series) => {
            update_subscription(subscription, series.snapshot(), state, today, messages);
            show_upcoming(&series, &subscription.name, today, messages);
        }
        Fetched::AniList(seasons) => {
            let snapshot = crate::anilist::anime_snapshot(&seasons).with_context(|| {
                format!(
                    "Failed to build release snapshot for AniList subscription '{}' ({})",
                    subscription.name, subscription.id
                )
            })?;

            let reminders = episode_reminders(&snapshot, &subscription.name, today);

            update_subscription(subscription, snapshot, state, today, messages);

            for notification in reminders {
                println!("    Reminder: {:?}", notification);
                messages.push(notification);
            }

            let updates = anime_updates(&seasons);
            if updates.is_empty() {
                println!("  No scheduled episodes or announced sequels.");
            } else {
                for update in updates {
                    println!("  {update}");
                }
            }
        }
    }

    Ok(())
}

pub(crate) async fn notify_and_save(
    notifier: &impl notifier::Notifier,
    messages: &[ReleaseNotification],
    state: &state::TrackerState,
    path: &std::path::Path,
) -> Result<()> {
    if let Err(failures) = send_all(notifier, messages).await {
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
    use std::{collections::BTreeMap, time::Duration};

    fn unused_bubble_client() -> BubbleClient {
        BubbleClient::new(reqwest::Client::new(), "http://unused.invalid".to_owned())
    }

    struct FakeNotifier {
        fail: bool,
    }

    impl notifier::Notifier for FakeNotifier {
        async fn send(&self, _message: &ReleaseNotification) -> Result<()> {
            if self.fail {
                anyhow::bail!("delivery failed");
            }
            Ok(())
        }
    }

    #[test]
    fn change_notifications_keep_release_metadata_and_previous_date() {
        use crate::release_date::ReleaseDate;

        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let old_date = ReleaseDate::Date(today + chrono::Duration::days(7));
        let new_date = ReleaseDate::Date(today + chrono::Duration::days(14));
        let record = state::ReleaseRecord {
            title: Some("T02 — Shin Taku".to_owned()),
            date: Some(old_date.clone()),
            image_url: Some("https://example.com/cover.jpg".to_owned()),
            url: Some("https://example.com/album".to_owned()),
        };
        let subscription = config::Subscription {
            provider: "bubble".to_owned(),
            id: "series".to_owned(),
            name: "Les 5 Terres".to_owned(),
        };
        let mut state = state::TrackerState::default();
        let mut notifications = Vec::new();
        update_subscription(
            &subscription,
            state::SeriesSnapshot {
                releases: BTreeMap::from([("album".to_owned(), record.clone())]),
            },
            &mut state,
            today,
            &mut notifications,
        );
        assert!(notifications.is_empty());
        let changed = state::ReleaseRecord {
            date: Some(new_date),
            ..record.clone()
        };
        update_subscription(
            &subscription,
            state::SeriesSnapshot {
                releases: BTreeMap::from([
                    ("album".to_owned(), changed.clone()),
                    ("new".to_owned(), record.clone()),
                ]),
            },
            &mut state,
            today,
            &mut notifications,
        );
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0].release, changed);
        assert!(matches!(
            &notifications[0].reason,
            NotificationReason::DateChanged { old_date: Some(date) } if date == &old_date
        ));
        assert_eq!(notifications[1].release, record);
        assert!(matches!(notifications[1].reason, NotificationReason::Added));
        for notification in &notifications {
            assert_eq!(notification.subscription_name, "Les 5 Terres");
        }
    }

    #[test]
    fn newly_announced_season_notifies_once() {
        use crate::release_date::ReleaseDate;

        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let subscription = config::Subscription {
            provider: "anilist".to_owned(),
            id: "1".to_owned(),
            name: "Jujutsu Kaisen".to_owned(),
        };
        let aired_season = state::ReleaseRecord {
            title: Some("Jujutsu Kaisen Season 3".to_owned()),
            date: Some(ReleaseDate::Date(today - chrono::Duration::days(300))),
            ..Default::default()
        };
        let announced_season = state::ReleaseRecord {
            title: Some("Jujutsu Kaisen Season 4".to_owned()),
            date: Some(ReleaseDate::Partial {
                year: Some(2027),
                month: None,
                day: None,
            }),
            image_url: Some("https://example.com/s4.jpg".to_owned()),
            url: Some("https://anilist.co/anime/2".to_owned()),
        };
        let before = BTreeMap::from([("season:1".to_owned(), aired_season.clone())]);
        let mut after = before.clone();
        after.insert("season:2".to_owned(), announced_season.clone());

        let mut state = state::TrackerState::default();
        let mut run = |releases: &BTreeMap<String, state::ReleaseRecord>| {
            let mut notifications = Vec::new();
            update_subscription(
                &subscription,
                state::SeriesSnapshot {
                    releases: releases.clone(),
                },
                &mut state,
                today,
                &mut notifications,
            );
            notifications
        };

        assert!(run(&before).is_empty(), "baseline must not notify");

        let announced = run(&after);
        assert_eq!(announced.len(), 1);
        assert_eq!(announced[0].subscription_name, "Jujutsu Kaisen");
        assert_eq!(announced[0].release, announced_season);
        assert!(matches!(announced[0].reason, NotificationReason::Added));

        assert!(
            run(&after).is_empty(),
            "unchanged rerun must not notify again"
        );
    }

    #[tokio::test]
    async fn anilist_http_error_retains_subscription_request_and_status() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .expect(2)
            .mount(&server)
            .await;

        let endpoint = server.uri();
        let error = {
            let client = AniListClient::new(reqwest::Client::new(), endpoint, Duration::ZERO);
            let subscription = config::Subscription {
                provider: "anilist".to_owned(),
                id: "123".to_owned(),
                name: "Test anime".to_owned(),
            };
            fetch_subscription(&subscription, &unused_bubble_client(), &client)
                .await
                .err()
                .expect("fetch should fail on repeated 429")
        };

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

    #[tokio::test]
    async fn saves_state_only_after_successful_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let previous = state::TrackerState::default();
        previous.save(&path).unwrap();

        let mut updated = state::TrackerState::default();
        updated
            .providers
            .insert("bubble".to_owned(), BTreeMap::new());
        let messages = vec![ReleaseNotification {
            subscription_name: "Example".to_owned(),
            release: state::ReleaseRecord {
                title: Some("New release".to_owned()),
                ..Default::default()
            },
            reason: NotificationReason::Added,
        }];

        let failing = FakeNotifier { fail: true };
        let error = notify_and_save(&failing, &messages, &updated, &path)
            .await
            .unwrap_err();
        let chain = format!("{error:#}");
        assert!(chain.contains("state was not saved"));
        assert!(chain.contains("Failed to send notification 1"));
        assert!(chain.contains("delivery failed"));
        assert_eq!(state::TrackerState::load(&path).unwrap(), previous);

        let successful = FakeNotifier { fail: false };
        notify_and_save(&successful, &messages, &updated, &path)
            .await
            .unwrap();
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

        let notifications = episode_reminders(&snapshot, "Example", today);
        assert_eq!(notifications.len(), 2);
        for (notification, id) in notifications.iter().zip(["episode:2", "episode:4"]) {
            assert_eq!(notification.subscription_name, "Example");
            assert_eq!(notification.release, snapshot.releases[id]);
        }
        assert!(matches!(notifications[0].reason, NotificationReason::Today));
        assert!(matches!(
            notifications[1].reason,
            NotificationReason::InOneWeek
        ));
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
        {
            let client = AniListClient::new(reqwest::Client::new(), endpoint, Duration::ZERO);
            let subscription = config::Subscription {
                provider: "anilist".to_owned(),
                id: "123".to_owned(),
                name: "Example".to_owned(),
            };
            let bubble_client = unused_bubble_client();
            let mut state = state::TrackerState::default();

            for _ in 0..2 {
                let mut messages = Vec::new();

                let fetched = fetch_subscription(&subscription, &bubble_client, &client)
                    .await
                    .unwrap();
                apply_subscription(&subscription, fetched, &mut state, today, &mut messages)
                    .unwrap();

                assert_eq!(messages.len(), 1);
                let notification = &messages[0];
                assert_eq!(notification.subscription_name, "Example");
                assert_eq!(
                    notification.release.title.as_deref(),
                    Some("Example — Episode 7")
                );
                assert_eq!(
                    notification.release.url.as_deref(),
                    Some("https://anilist.co/anime/123")
                );
                assert_eq!(
                    notification.release.date,
                    Some(crate::release_date::ReleaseDate::Instant(
                        chrono::DateTime::from_timestamp(airing_at, 0).unwrap()
                    ))
                );
                assert!(matches!(notification.reason, NotificationReason::Today));
            }
        };

        server.verify().await;
    }
}
