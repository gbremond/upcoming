use reqwest::blocking::Client;

pub(crate) fn send_all(
    notifier: &dyn Notifier,
    messages: &[String],
) -> Result<(), Vec<(usize, Box<dyn std::error::Error>)>> {
    let mut failures = Vec::new();

    for (index, message) in messages.iter().enumerate() {
        if let Err(error) = notifier.send(message) {
            failures.push((index, error));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

pub(crate) trait Notifier {
    fn send(&self, message: &str) -> Result<(), Box<dyn std::error::Error>>;
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
    fn send(&self, message: &str) -> Result<(), Box<dyn std::error::Error>> {
        let body = serde_json::json!({
            "content": message,
            "allowed_mentions": { "parse": [] }
        });

        self.http
            .post(&self.webhook_url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .map_err(|error| error.without_url())?
            .error_for_status()
            .map_err(|error| error.without_url())?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn sends_a_discord_message() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/webhook"))
            .and(body_json(serde_json::json!({
                "content": "A new release!",
                "allowed_mentions": { "parse": [] }
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let url = format!("{}/webhook", server.uri());

        tokio::task::spawn_blocking(move || {
            let client: &dyn Notifier = &DiscordClient::new(Client::new(), url);
            client.send("A new release!").unwrap();
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
            let error = client.send("A new release!").unwrap_err();

            let status = error
                .downcast_ref::<reqwest::Error>()
                .expect("expected a Reqwest error")
                .status();

            (status, error.to_string())
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
            fn send(&self, message: &str) -> Result<(), Box<dyn std::error::Error>> {
                self.attempted.borrow_mut().push(message.to_owned());

                if message == "second" {
                    return Err(std::io::Error::other("delivery failed").into());
                }

                Ok(())
            }
        }

        let notifier = TestNotifier {
            attempted: RefCell::new(Vec::new()),
        };
        let messages = vec!["first".to_owned(), "second".to_owned(), "third".to_owned()];

        let failures = send_all(&notifier, &messages).unwrap_err();

        assert_eq!(
            *notifier.attempted.borrow(),
            vec!["first", "second", "third"]
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, 1); // The second message failed.
    }
}
