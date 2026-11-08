mod discord;
mod notification;
mod payload;

pub(crate) use discord::DiscordClient;
pub(crate) use notification::{NotificationReason, ReleaseNotification};

use anyhow::Result;

pub(crate) async fn send_all(
    notifier: &impl Notifier,
    notifications: &[ReleaseNotification],
) -> Result<(), Vec<(usize, anyhow::Error)>> {
    let mut failures = Vec::new();

    for (index, notification) in notifications.iter().enumerate() {
        if let Err(error) = notifier.send(notification).await {
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
    async fn send(&self, notification: &ReleaseNotification) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifier::notification::test_support::sample_notification;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn attempts_remaining_messages_after_a_failure() {
        use std::cell::RefCell;

        struct TestNotifier {
            attempted: RefCell<Vec<String>>,
        }

        impl Notifier for TestNotifier {
            async fn send(&self, notification: &ReleaseNotification) -> Result<()> {
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
        let notifications: Vec<_> = ["first", "second", "third"]
            .into_iter()
            .map(|name| ReleaseNotification {
                subscription_name: name.to_owned(),
                ..sample_notification()
            })
            .collect();

        let failures = send_all(&notifier, &notifications).await.unwrap_err();

        assert_eq!(
            *notifier.attempted.borrow(),
            vec!["first", "second", "third"]
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, 1); // The second notification failed.
    }
}
