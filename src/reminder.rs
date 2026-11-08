use crate::bubble::BubbleSeries;
use crate::notifier::{NotificationReason, ReleaseNotification};
use crate::state::SeriesSnapshot;
use chrono::NaiveDate;

pub(crate) fn album_reminders(
    series: &BubbleSeries,
    subscription_name: &str,
    today: NaiveDate,
) -> Vec<ReleaseNotification> {
    series
        .upcoming_albums(today)
        .into_iter()
        .filter_map(|(album, date)| {
            let reason = reminder_reason(date, today)?;
            Some(ReleaseNotification {
                subscription_name: subscription_name.to_owned(),
                release: album.release_record(),
                reason,
            })
        })
        .collect()
}

pub(crate) fn episode_reminders(
    snapshot: &SeriesSnapshot,
    subscription_name: &str,
    today: NaiveDate,
) -> Vec<ReleaseNotification> {
    snapshot
        .releases
        .iter()
        .filter(|(id, _)| id.starts_with("episode:"))
        .filter_map(|(_, record)| {
            let date = record.date.as_ref()?.reminder_date()?;
            let reason = reminder_reason(date, today)?;
            Some(ReleaseNotification {
                subscription_name: subscription_name.to_owned(),
                release: record.clone(),
                reason,
            })
        })
        .collect()
}

fn reminder_reason(release_date: NaiveDate, today: NaiveDate) -> Option<NotificationReason> {
    match reminder_for(release_date, today)? {
        Reminder::Today => Some(NotificationReason::Today),
        Reminder::InOneWeek => Some(NotificationReason::InOneWeek),
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Reminder {
    Today,
    InOneWeek,
}

pub(crate) fn reminder_for(release_date: NaiveDate, today: NaiveDate) -> Option<Reminder> {
    let days = (release_date - today).num_days();

    match days {
        0 => Some(Reminder::Today),
        7 => Some(Reminder::InOneWeek),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminds_only_on_release_day_and_one_week_before() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 7).unwrap();

        for (day, expected) in [
            (6, None), // yesterday
            (7, Some(Reminder::Today)),
            (13, None), // six days away
            (14, Some(Reminder::InOneWeek)),
            (15, None), // eight days away
        ] {
            let release = NaiveDate::from_ymd_opt(2026, 11, day).unwrap();
            assert_eq!(reminder_for(release, today), expected);
        }
    }

    #[test]
    fn album_reminders_preserve_series_tome_cover_link_and_timing() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let series: crate::bubble::BubbleSeries = serde_json::from_value(serde_json::json!({
            "albums": [
                {
                    "objectId": "today", "title": "Shin Taku", "tome": 2,
                    "prints": [{"publicationDate": "2026-11-08T00:00:00Z"}],
                    "images": {"front": {"medium": "https://example.com/cover.jpg"}},
                    "permalink": "les-5-terres", "defaultSellingPrintObjectId": "print-2"
                },
                {
                    "objectId": "week", "title": "Next tome", "tome": 3,
                    "prints": [{"publicationDate": "2026-11-15T00:00:00Z"}]
                },
                {
                    "objectId": "later", "title": "Later", "tome": 4,
                    "prints": [{"publicationDate": "2026-11-16T00:00:00Z"}]
                }
            ]
        }))
        .unwrap();
        let notifications = album_reminders(&series, "Les 5 Terres (hors série)", today);
        assert_eq!(notifications.len(), 2);
        for notification in &notifications {
            assert_eq!(notification.subscription_name, "Les 5 Terres (hors série)");
        }
        assert_eq!(
            notifications[0].release.title.as_deref(),
            Some("T02 — Shin Taku")
        );
        assert_eq!(
            notifications[0].release.image_url.as_deref(),
            Some("https://example.com/cover.jpg")
        );
        assert_eq!(
            notifications[0].release.url.as_deref(),
            Some("https://www.bubblebd.com/les-5-terres/album/today/print-2")
        );
        assert!(matches!(notifications[0].reason, NotificationReason::Today));
        assert!(matches!(
            notifications[1].reason,
            NotificationReason::InOneWeek
        ));
        assert_eq!(
            notifications[0].release,
            series.snapshot().releases["today"]
        );
        assert_eq!(notifications[1].release, series.snapshot().releases["week"]);
    }

    #[test]
    fn anime_reminders_cover_episodes_but_not_seasons() {
        use crate::release_date::ReleaseDate;
        use crate::state::{ReleaseRecord, SeriesSnapshot};
        use chrono::TimeZone;

        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        // Local noon keeps the fixture independent of the machine's UTC offset.
        let airs_today = chrono::Local
            .from_local_datetime(&today.and_hms_opt(12, 0, 0).unwrap())
            .single()
            .unwrap()
            .with_timezone(&chrono::Utc);
        let episode = ReleaseRecord {
            title: Some("Frieren — Episode 7".to_owned()),
            date: Some(ReleaseDate::Instant(airs_today)),
            ..Default::default()
        };
        let season = ReleaseRecord {
            title: Some("Frieren Season 2".to_owned()),
            date: Some(ReleaseDate::Date(today)),
            ..Default::default()
        };
        let snapshot = SeriesSnapshot {
            releases: [
                ("episode:7".to_owned(), episode.clone()),
                ("season:2".to_owned(), season),
            ]
            .into(),
        };

        let reminders = episode_reminders(&snapshot, "Frieren", today);

        assert_eq!(
            reminders.len(),
            1,
            "a season starting today is not reminded"
        );
        assert_eq!(reminders[0].subscription_name, "Frieren");
        assert_eq!(reminders[0].release, episode);
        assert!(matches!(reminders[0].reason, NotificationReason::Today));
    }
}
