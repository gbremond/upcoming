use super::payload::discord_payload;
use super::{Notifier, ReleaseNotification};
use anyhow::{Context, Result};
use reqwest::Client;

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
    async fn send(&self, notification: &ReleaseNotification) -> Result<()> {
        let body = discord_payload(notification)?;

        self.http
            .post(&self.webhook_url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|error| error.without_url())
            .context("Failed to send Discord webhook request")?
            .error_for_status()
            .map_err(|error| error.without_url())
            .context("Discord webhook returned an HTTP error")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifier::NotificationReason;
    use crate::notifier::notification::test_support::sample_notification;
    use crate::release_date::ReleaseDate;
    use crate::state::ReleaseRecord;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn oversized_title_is_rejected_without_posting_to_discord() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        let url = format!("{}/webhook/test-secret", server.uri());
        let error = {
            let client = DiscordClient::new(Client::new(), url);
            let mut notification = sample_notification();
            notification.release.title = Some("x".repeat(257));
            client.send(&notification).await.unwrap_err()
        };
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

        DiscordClient::new(Client::new(), url)
            .send(&sample_notification())
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

        let (status, error_message) = {
            let client = DiscordClient::new(Client::new(), url);
            let error = client.send(&sample_notification()).await.unwrap_err();

            let status = error
                .downcast_ref::<reqwest::Error>()
                .expect("expected a Reqwest error")
                .status();

            (status, format!("{error:#?}"))
        };

        assert_eq!(status, Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!error_message.contains("test-secret"));

        server.verify().await;
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
        {
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
            client.send(&notification).await.unwrap();
        };
        server.verify().await;
    }
}
