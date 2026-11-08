use crate::release_date::ReleaseDate;
use crate::state::ReleaseRecord;
use anyhow::{Context, Result};
use reqwest::blocking::Client;

pub(crate) fn send_all(
    notifier: &dyn Notifier,
    messages: &[ReleaseNotification],
) -> Result<(), Vec<(usize, anyhow::Error)>> {
    let mut failures = Vec::new();

    for (index, message) in messages.iter().enumerate() {
        if let Err(error) = notifier.send(message) {
            failures.push((
                index,
                error.context(format!("Failed to send notification {}", index + 1)),
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

pub(crate) trait Notifier {
    fn send(&self, message: &ReleaseNotification) -> Result<()>;
}

pub(crate) struct DiscordClient {
    http: Client,
    webhook_url: String,
}

impl DiscordClient {
    pub(crate) fn new(http: Client, webhook_url: String) -> Self {
        Self { http, webhook_url }
    }
}

impl Notifier for DiscordClient {
    fn send(&self, notification: &ReleaseNotification) -> Result<()> {
        let body = discord_payload(notification)?;

        self.http
            .post(&self.webhook_url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .map_err(|error| error.without_url())
            .context("Failed to send Discord webhook request")?
            .error_for_status()
            .map_err(|error| error.without_url())
            .context("Discord webhook returned an HTTP error")?;

        Ok(())
    }
}

fn discord_payload(notification: &ReleaseNotification) -> Result<serde_json::Value> {
    let description = match &notification.reason {
        NotificationReason::Added => "✨ New release announced",
        NotificationReason::DateChanged { .. } => "📅 Release date changed",
        NotificationReason::Today => "🎉 Releases today",
        NotificationReason::InOneWeek => "⏳ Releases in one week",
    };

    let title = notification
        .release
        .title
        .as_deref()
        .unwrap_or(&notification.subscription_name);

    // UTF-16 counting is conservative for Discord clients, including emoji.
    anyhow::ensure!(
        !title.trim().is_empty() && title.encode_utf16().count() <= 256,
        "Discord embed title must contain 1-256 UTF-16 units; shorten the release title for subscription '{}'",
        notification.subscription_name
    );
    anyhow::ensure!(
        !notification.subscription_name.trim().is_empty()
            && notification.subscription_name.encode_utf16().count() <= 256,
        "Discord embed author must contain 1-256 UTF-16 units; shorten the subscription name"
    );

    let mut embed = serde_json::json!({
        "title": title,
        "description": description,
        "color": 0xFFA500,
        "author": {
            "name": notification.subscription_name
        }
    });

    if let Some(url) = &notification.release.url {
        embed["url"] = serde_json::json!(url);
    }

    if let Some(image_url) = &notification.release.image_url {
        embed["thumbnail"] = serde_json::json!({
            "url": image_url
        });
    }

    let mut fields = vec![serde_json::json!({
        "name": "Release date",
        "value": format_release_date(notification.release.date.as_ref()),
        "inline": true
    })];

    if let NotificationReason::DateChanged { old_date } = &notification.reason {
        fields.push(serde_json::json!({
            "name": "Previous date",
            "value": format_release_date(old_date.as_ref()),
            "inline": true
        }));
    }

    embed["fields"] = serde_json::json!(fields);

    // Only title/author are unbounded inputs; fixed descriptions and date fields
    // keep the single embed comfortably below Discord's 6,000-character total.
    Ok(serde_json::json!({
        "embeds": [embed],
        "allowed_mentions": { "parse": [] }
    }))
}

#[derive(Debug)]
pub(crate) struct ReleaseNotification {
    pub(crate) subscription_name: String,
    pub(crate) release: ReleaseRecord,
    pub(crate) reason: NotificationReason,
}

#[derive(Debug)]
pub(crate) enum NotificationReason {
    Added,
    DateChanged { old_date: Option<ReleaseDate> },
    Today,
    InOneWeek,
}

fn format_release_date(date: Option<&ReleaseDate>) -> String {
    match date {
        None => "Not announced".to_owned(),
        Some(ReleaseDate::Date(date)) => date.format("%d/%m/%Y").to_string(),
        Some(ReleaseDate::Instant(instant)) => instant
            .with_timezone(&chrono::Local)
            .format("%d/%m/%Y at %H:%M %:z")
            .to_string(),
        Some(ReleaseDate::Partial { year, month, day }) => {
            format!(
                "{}-{}-{} (partial date)",
                year.map(|value| value.to_string())
                    .unwrap_or_else(|| "????".to_owned()),
                month
                    .map(|value| format!("{value:02}"))
                    .unwrap_or_else(|| "??".to_owned()),
                day.map(|value| format!("{value:02}"))
                    .unwrap_or_else(|| "??".to_owned()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_notification() -> ReleaseNotification {
        ReleaseNotification {
            subscription_name: "Example".to_owned(),
            release: ReleaseRecord {
                title: Some("Episode 7".to_owned()),
                date: Some(ReleaseDate::Date(
                    chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
                )),
                image_url: Some("https://example.com/cover.jpg".to_owned()),
                url: Some("https://anilist.co/anime/123".to_owned()),
            },
            reason: NotificationReason::Today,
        }
    }

    #[test]
    fn payload_describes_every_reason_and_preserves_dates() {
        let date = ReleaseDate::Date(chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
        for (reason, description, previous_date) in [
            (NotificationReason::Added, "✨ New release announced", None),
            (NotificationReason::Today, "🎉 Releases today", None),
            (
                NotificationReason::InOneWeek,
                "⏳ Releases in one week",
                None,
            ),
            (
                NotificationReason::DateChanged {
                    old_date: Some(date),
                },
                "📅 Release date changed",
                Some("01/11/2026"),
            ),
        ] {
            let notification = ReleaseNotification {
                reason,
                ..sample_notification()
            };
            let payload = discord_payload(&notification).unwrap();
            let embed = &payload["embeds"][0];
            assert_eq!(embed["description"], description);
            assert_eq!(embed["fields"][0]["value"], "08/11/2026");
            let fields = embed["fields"].as_array().unwrap();
            if let Some(previous) = previous_date {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[1]["name"], "Previous date");
                assert_eq!(fields[1]["value"], previous);
            } else {
                assert_eq!(fields.len(), 1);
            }
            assert_eq!(
                payload["allowed_mentions"],
                serde_json::json!({"parse": []})
            );
        }
    }

    #[test]
    fn payload_uses_subscription_as_author_without_duplicating_season_title() {
        let mut notification = sample_notification();
        notification.subscription_name = "Apothecary Diaries".to_owned();
        notification.release.title = Some("The Apothecary Diaries Season 3 — Episode 7".to_owned());
        let payload = discord_payload(&notification).unwrap();
        let embed = &payload["embeds"][0];
        assert_eq!(embed["author"]["name"], "Apothecary Diaries");
        assert_eq!(
            embed["title"],
            "The Apothecary Diaries Season 3 — Episode 7"
        );
        assert!(embed.get("image").is_none());
        assert!(embed.get("footer").is_none());
        assert!(embed.get("thumbnail").is_some());
    }

    #[test]
    fn payload_validates_title_and_author_limits_without_splitting_unicode() {
        let mut notification = sample_notification();
        notification.release.title = Some("é".repeat(256));
        notification.subscription_name = "a".repeat(256);
        assert!(discord_payload(&notification).is_ok());

        notification.release.title = Some("é".repeat(257));
        assert!(
            discord_payload(&notification)
                .unwrap_err()
                .to_string()
                .contains("title")
        );
        notification.release.title = Some("🎉".repeat(128));
        assert!(discord_payload(&notification).is_ok());
        notification.release.title = Some("🎉".repeat(129));
        assert!(discord_payload(&notification).is_err());

        notification.release.title = Some("Episode 7".to_owned());
        notification.subscription_name = "a".repeat(257);
        assert!(
            discord_payload(&notification)
                .unwrap_err()
                .to_string()
                .contains("author")
        );
        notification.subscription_name = "Example".to_owned();
        notification.release.title = Some(" ".to_owned());
        assert!(discord_payload(&notification).is_err());
    }

    #[tokio::test]
    async fn oversized_title_is_rejected_without_posting_to_discord() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        let url = format!("{}/webhook/test-secret", server.uri());
        let error = tokio::task::spawn_blocking(move || {
            let client = DiscordClient::new(Client::new(), url);
            let mut notification = sample_notification();
            notification.release.title = Some("x".repeat(257));
            client.send(&notification).unwrap_err()
        })
        .await
        .unwrap();
        assert!(error.to_string().contains("shorten the release title"));
        assert!(!format!("{error:#?}").contains("test-secret"));
        server.verify().await;
    }

    #[tokio::test]
    async fn sends_a_discord_message() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/webhook"))
            .and(body_json(serde_json::json!({
                "embeds": [{
                    "title": "Episode 7",
                    "description": "🎉 Releases today",
                    "color": 0xFFA500,
                    "author": {"name": "Example"},
                    "url": "https://anilist.co/anime/123",
                    "thumbnail": {"url": "https://example.com/cover.jpg"},
                    "fields": [{
                        "name": "Release date",
                        "value": "08/11/2026",
                        "inline": true
                    }]
                }],
                "allowed_mentions": { "parse": [] }
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let url = format!("{}/webhook", server.uri());

        tokio::task::spawn_blocking(move || {
            let client: &dyn Notifier = &DiscordClient::new(Client::new(), url);
            client.send(&sample_notification()).unwrap();
        })
        .await
        .unwrap();

        server.verify().await;
    }

    #[tokio::test]
    async fn reports_http_failure_without_exposing_webhook_url() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/webhook/test-secret"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;

        let url = format!("{}/webhook/test-secret", server.uri());

        let (status, error_message) = tokio::task::spawn_blocking(move || {
            let client = DiscordClient::new(Client::new(), url);
            let error = client.send(&sample_notification()).unwrap_err();

            let status = error
                .downcast_ref::<reqwest::Error>()
                .expect("expected a Reqwest error")
                .status();

            (status, format!("{error:#?}"))
        })
        .await
        .unwrap();

        assert_eq!(status, Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!error_message.contains("test-secret"));

        server.verify().await;
    }

    #[test]
    fn attempts_remaining_messages_after_a_failure() {
        use std::cell::RefCell;

        struct TestNotifier {
            attempted: RefCell<Vec<String>>,
        }

        impl Notifier for TestNotifier {
            fn send(&self, notification: &ReleaseNotification) -> Result<()> {
                self.attempted
                    .borrow_mut()
                    .push(notification.subscription_name.clone());

                if notification.subscription_name == "second" {
                    anyhow::bail!("delivery failed");
                }

                Ok(())
            }
        }

        let notifier = TestNotifier {
            attempted: RefCell::new(Vec::new()),
        };
        let messages: Vec<_> = ["first", "second", "third"]
            .into_iter()
            .map(|name| ReleaseNotification {
                subscription_name: name.to_owned(),
                ..sample_notification()
            })
            .collect();

        let failures = send_all(&notifier, &messages).unwrap_err();

        assert_eq!(
            *notifier.attempted.borrow(),
            vec!["first", "second", "third"]
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, 1); // The second message failed.
    }

    #[tokio::test]
    async fn sends_date_change_embed_with_previous_date_and_missing_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/webhook"))
            .and(body_json(serde_json::json!({
                "embeds": [{
                    "title": "Example",
                    "description": "📅 Release date changed",
                    "color": 0xFFA500,
                    "author": {"name": "Example"},
                    "fields": [
                        {
                            "name": "Release date",
                            "value": "2027-10-?? (partial date)",
                            "inline": true
                        },
                        {
                            "name": "Previous date",
                            "value": "Not announced",
                            "inline": true
                        }
                    ]
                }],
                "allowed_mentions": {"parse": []}
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let url = format!("{}/webhook", server.uri());
        tokio::task::spawn_blocking(move || {
            let client = DiscordClient::new(Client::new(), url);
            let notification = ReleaseNotification {
                subscription_name: "Example".to_owned(),
                release: ReleaseRecord {
                    date: Some(ReleaseDate::Partial {
                        year: Some(2027),
                        month: Some(10),
                        day: None,
                    }),
                    ..Default::default()
                },
                reason: NotificationReason::DateChanged { old_date: None },
            };
            client.send(&notification).unwrap();
        })
        .await
        .unwrap();
        server.verify().await;
    }

    #[test]
    fn formats_episode_instant_in_local_time_with_offset() {
        let instant: chrono::DateTime<chrono::Utc> = "2026-11-08T18:30:00Z".parse().unwrap();
        assert_eq!(
            format_release_date(Some(&ReleaseDate::Instant(instant))),
            instant
                .with_timezone(&chrono::Local)
                .format("%d/%m/%Y at %H:%M %:z")
                .to_string()
        );
    }
}
