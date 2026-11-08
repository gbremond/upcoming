use crate::anilist::{AniListClient, Anime};
use crate::bubble::{BubbleClient, BubbleSeries};
use crate::config;
use crate::notifier::{self, NotificationReason, ReleaseNotification, send_all};
use crate::reminder::{album_reminders, episode_reminders};
use crate::state;
use anyhow::{Context, Result};
use futures::{StreamExt, stream::FuturesUnordered};

/// One full tracker pass: fetch subscriptions concurrently and process results
/// in completion order, notifying and saving each successful scan.
pub(crate) async fn run(
    subscriptions: &[config::Subscription],
    bubble_client: &BubbleClient,
    anilist_client: &AniListClient,
    notifier: &impl notifier::Notifier,
    state_path: &std::path::Path,
    today: chrono::NaiveDate,
) -> Result<()> {
    let started = std::time::Instant::now();
    let mut state = state::TrackerState::load(state_path)?;

    let mut fetches: FuturesUnordered<_> = subscriptions
        .iter()
        .map(|subscription| async move {
            let result = fetch_subscription(subscription, bubble_client, anilist_client).await;

            (subscription, result)
        })
        .collect();

    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

    let fetching = async move {
        while let Some(result) = fetches.next().await {
            if sender.send(result).await.is_err() {
                break; // Processor exited; stop fetching.
            }
        }
    };

    let processing = async move {
        let mut failed_subscriptions = 0;
        let mut failed_notifications = 0;
        let mut notification_count = 0;

        while let Some((subscription, fetched)) = receiver.recv().await {
            let mut notifications = Vec::new();

            let result = fetched.and_then(|fetched| {
                apply_subscription(subscription, fetched, &mut state, today, &mut notifications)
            });

            if let Err(error) = result {
                failed_subscriptions += 1;
                tracing::error!(
                    sub = %subscription.name,
                    "subscription scan failed: {error:#}"
                );
                continue;
            }

            notification_count += notifications.len();

            if let Err(failures) = send_all(notifier, &notifications).await {
                failed_notifications += failures.len();

                for (index, error) in failures {
                    tracing::error!(
                        sub = %subscription.name,
                        notification = index + 1,
                        "delivery failed: {error:#}"
                    );
                }
            }

            state
                .save(state_path)
                .with_context(|| format!("Failed to save state after '{}'", subscription.name))?;
        }

        tracing::info!(
            subscriptions = subscriptions.len(),
            notifications = notification_count,
            failed_subscriptions,
            failed_notifications,
            elapsed = ?started.elapsed(),
            "tracker scan finished"
        );

        if failed_subscriptions > 0 || failed_notifications > 0 {
            anyhow::bail!(
                "{failed_subscriptions} subscription scan(s) and \
             {failed_notifications} notification(s) failed; \
             successful scans were saved"
            );
        }

        Ok(())
    };

    let (_, result) = tokio::join!(fetching, processing);
    result
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
    match fetched {
        FetchedSeries::Bubble(series) => {
            let reminders = album_reminders(&series, &subscription.name, today);
            collect_changes(subscription, series.snapshot(), state, today, notifications);
            notifications.extend(reminders);
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
            notifications.extend(reminders);
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    fn unused_bubble_client() -> BubbleClient {
        BubbleClient::new(reqwest::Client::new(), "http://unused.invalid".to_owned())
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
}
