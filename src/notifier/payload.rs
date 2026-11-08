use super::{NotificationReason, ReleaseNotification};
use crate::release_date::ReleaseDate;
use anyhow::Result;

pub(super) fn discord_payload(notification: &ReleaseNotification) -> Result<serde_json::Value> {
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
    use crate::notifier::notification::test_support::sample_notification;
    use pretty_assertions::assert_eq;

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
