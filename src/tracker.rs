use crate::anilist::{AniListClient, Anime};
use crate::bubble::{BubbleClient, BubbleSeries};
use crate::config;
use crate::console::{album_overview, anime_overview, print_reminders, print_section};
use crate::notifier::{self, NotificationReason, ReleaseNotification, send_all};
use crate::reminder::{album_reminders, episode_reminders};
use crate::state;
use anyhow::{Context, Result};

/// One full tracker pass: fetch every subscription concurrently, apply the
/// results in config order, then notify and save.
pub(crate) async fn run(
    subscriptions: &[config::Subscription],
    bubble_client: &BubbleClient,
    anilist_client: &AniListClient,
    notifier: &impl notifier::Notifier,
    state_path: &std::path::Path,
    today: chrono::NaiveDate,
) -> Result<()> {
    let mut state = state::TrackerState::load(state_path)?;
    let mut notifications = Vec::new();

    let results = futures::future::join_all(
        subscriptions
            .iter()
            .map(|subscription| fetch_subscription(subscription, bubble_client, anilist_client)),
    )
    .await;

    for (subscription, fetched) in subscriptions.iter().zip(results) {
        apply_subscription(
            subscription,
            fetched?,
            &mut state,
            today,
            &mut notifications,
        )?;
    }

    notify_and_save(notifier, &notifications, &state, state_path).await
}

pub(crate) enum FetchedSeries {
    Bubble(BubbleSeries),
    AniList(Vec<Anime>),
}

#[tracing::instrument(
    name = "fetch",
    skip_all,
    fields(provider = %subscription.provider, sub = %subscription.name)
)]
pub(crate) async fn fetch_subscription(
    subscription: &config::Subscription,
    bubble_client: &BubbleClient,
    anilist_client: &AniListClient,
) -> Result<FetchedSeries> {
    let started = std::time::Instant::now();
    tracing::info!(id = %subscription.id, "fetching");

    let fetched = match subscription.provider.as_str() {
        "bubble" => {
            let series = bubble_client
                .fetch_series(&subscription.id)
                .await
                .with_context(|| {
                    format!(
                        "Failed to fetch Bubble subscription '{}' ({})",
                        subscription.name, subscription.id
                    )
                })?;
            Ok(FetchedSeries::Bubble(series))
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
            Ok(FetchedSeries::AniList(seasons))
        }
        provider => anyhow::bail!(
            "Unsupported provider '{provider}' for subscription '{}'",
            subscription.name
        ),
    };

    if fetched.is_ok() {
        tracing::info!(elapsed = ?started.elapsed(), "fetched");
    }

    fetched
}

pub(crate) fn apply_subscription(
    subscription: &config::Subscription,
    fetched: FetchedSeries,
    state: &mut state::TrackerState,
    today: chrono::NaiveDate,
    notifications: &mut Vec<ReleaseNotification>,
) -> Result<()> {
    println!(
        "=== {}: {} ({}) ===",
        subscription.provider, subscription.name, subscription.id
    );

    match fetched {
        FetchedSeries::Bubble(series) => {
            let reminders = album_reminders(&series, &subscription.name, today);
            collect_changes(subscription, series.snapshot(), state, today, notifications);
            print_reminders(&reminders);
            notifications.extend(reminders);

            print_section(
                &album_overview(&series, today),
                "No upcoming albums with known publication dates.",
            );
        }
        FetchedSeries::AniList(seasons) => {
            let snapshot = crate::anilist::anime_snapshot(&seasons).with_context(|| {
                format!(
                    "Failed to build release snapshot for AniList subscription '{}' ({})",
                    subscription.name, subscription.id
                )
            })?;

            let reminders = episode_reminders(&snapshot, &subscription.name, today);
            collect_changes(subscription, snapshot, state, today, notifications);
            print_reminders(&reminders);
            notifications.extend(reminders);

            print_section(
                &anime_overview(&seasons),
                "No scheduled episodes or announced sequels.",
            );
        }
    }

    Ok(())
}

fn collect_changes(
    subscription: &config::Subscription,
    snapshot: state::SeriesSnapshot,
    state: &mut state::TrackerState,
    today: chrono::NaiveDate,
    notifications: &mut Vec<ReleaseNotification>,
) {
    match state.record_snapshot(&subscription.provider, &subscription.id, snapshot) {
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

                notifications.push(ReleaseNotification {
                    subscription_name: subscription.name.clone(),
                    release,
                    reason,
                });
            }
        }
        None => tracing::info!(
            sub = %subscription.name,
            "first run for this subscription: saving baseline"
        ),
    }
}

pub(crate) async fn notify_and_save(
    notifier: &impl notifier::Notifier,
    notifications: &[ReleaseNotification],
    state: &state::TrackerState,
    path: &std::path::Path,
) -> Result<()> {
    if let Err(failures) = send_all(notifier, notifications).await {
        for (index, error) in &failures {
            tracing::error!(notification = index + 1, "delivery failed: {error:#}");
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
    use pretty_assertions::assert_eq;
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
        let notifications = vec![ReleaseNotification {
            subscription_name: "Example".to_owned(),
            release: state::ReleaseRecord {
                title: Some("New release".to_owned()),
                ..Default::default()
            },
            reason: NotificationReason::Added,
        }];

        let failing = FakeNotifier { fail: true };
        let error = notify_and_save(&failing, &notifications, &updated, &path)
            .await
            .unwrap_err();
        let chain = format!("{error:#}");
        assert!(chain.contains("state was not saved"));
        assert!(chain.contains("Failed to send notification 1"));
        assert!(chain.contains("delivery failed"));
        assert_eq!(state::TrackerState::load(&path).unwrap(), previous);

        let successful = FakeNotifier { fail: false };
        notify_and_save(&successful, &notifications, &updated, &path)
            .await
            .unwrap();
        assert_eq!(state::TrackerState::load(&path).unwrap(), updated);
    }
}
