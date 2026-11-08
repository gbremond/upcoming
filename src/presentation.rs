use crate::notifier::{NotificationReason, ReleaseNotification};
use crate::reminder::{Reminder, reminder_for};
use crate::{anilist, bubble};

pub(crate) fn anime_updates(seasons: &[anilist::Anime]) -> Vec<String> {
    let mut updates = Vec::new();
    let mut announced = std::collections::HashSet::new();

    for season in seasons {
        if let Some(episode) = &season.next_airing_episode {
            let local_time = episode.airing_at.with_timezone(&chrono::Local);
            updates.push(format!(
                "{} — episode {} airs at {}",
                season.title(),
                episode.episode,
                local_time.format("%d/%m/%Y %H:%M %:z")
            ));
        }

        for sequel in season.announced_sequels() {
            if !announced.insert(sequel.id) {
                continue;
            }
            updates.push(format!(
                "❗ Announced sequel: {} ({}) — {}",
                sequel.title(),
                sequel.id,
                sequel.release_window()
            ));
        }
    }

    updates
}

pub(crate) fn show_upcoming(
    series: &bubble::BubbleSeries,
    subscription_name: &str,
    today: chrono::NaiveDate,
    messages: &mut Vec<ReleaseNotification>,
) {
    let upcoming = series.upcoming_albums(today);

    if upcoming.is_empty() {
        println!("  No upcoming albums with known publication dates.");
        return;
    }

    for (album, date) in upcoming {
        let tome = match album.tome {
            Some(number) => format!("T{number:02}"),
            None => "Unnumbered".to_owned(),
        };

        let title = album.title.as_deref().unwrap_or("(untitled)");
        let days = (date - today).num_days();

        let timing = if days == 0 {
            "today".to_owned()
        } else {
            format!("in {days} days")
        };

        println!(
            " ❗ {} | {tome} | {title} | {timing}",
            date.format("%d/%m/%Y")
        );

        if let Some(reminder) = reminder_for(date, today) {
            let description = match reminder {
                Reminder::Today => "releases today",
                Reminder::InOneWeek => "releases in one week",
            };

            let message = format!("{subscription_name} — {tome} — {title} {description} ({date})");
            println!("    Reminder: {message}");
            messages.push(ReleaseNotification {
                subscription_name: subscription_name.to_owned(),
                release: album.release_record(),
                reason: match reminder {
                    Reminder::Today => NotificationReason::Today,
                    Reminder::InOneWeek => NotificationReason::InOneWeek,
                },
            });
        }
    }
}

pub(crate) fn episode_reminders(
    snapshot: &crate::state::SeriesSnapshot,
    subscription_name: &str,
    today: chrono::NaiveDate,
) -> Vec<ReleaseNotification> {
    let mut notifications = Vec::new();

    for (id, record) in &snapshot.releases {
        if !id.starts_with("episode:") {
            continue;
        }

        let Some(date) = record.date.as_ref().and_then(|date| date.reminder_date()) else {
            continue;
        };

        let reason = match reminder_for(date, today) {
            Some(Reminder::Today) => NotificationReason::Today,
            Some(Reminder::InOneWeek) => NotificationReason::InOneWeek,
            None => continue,
        };

        notifications.push(ReleaseNotification {
            subscription_name: subscription_name.to_owned(),
            release: record.clone(),
            reason,
        });
    }

    notifications
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bubble_reminders_preserve_series_tome_cover_link_and_timing() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let series: bubble::BubbleSeries = serde_json::from_value(serde_json::json!({
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
        let mut notifications = Vec::new();
        show_upcoming(
            &series,
            "Les 5 Terres (hors série)",
            today,
            &mut notifications,
        );
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

    #[test]
    fn anime_overview_shows_each_announcement_once() {
        let sequel = serde_json::json!({
            "relationType": "SEQUEL",
            "node": {
                "id": 3,
                "type": "ANIME",
                "status": "NOT_YET_RELEASED",
                "title": {"romaji": "Season 3"},
                "startDate": {"year": 2027, "month": 10, "day": null}
            }
        });
        let seasons: Vec<anilist::Anime> = serde_json::from_value(serde_json::json!([
            {
                "id": 1, "title": {"romaji": "Season 1"},
                "relations": {"edges": [sequel.clone()]}
            },
            {
                "id": 2, "title": {"romaji": "Season 2"},
                "relations": {"edges": [sequel]}
            },
            {"id": 3, "title": {"romaji": "Season 3"}}
        ]))
        .unwrap();

        assert_eq!(
            anime_updates(&seasons),
            vec!["❗ Announced sequel: Season 3 (3) — 2027-10 (day unknown)"]
        );
    }

    #[test]
    fn anime_overview_includes_scheduled_episodes() {
        let seasons: Vec<anilist::Anime> = serde_json::from_value(serde_json::json!([{
            "id": 1,
            "title": {"romaji": "Example"},
            "nextAiringEpisode": {"episode": 7, "airingAt": 0}
        }]))
        .unwrap();

        let updates = anime_updates(&seasons);
        assert_eq!(updates.len(), 1);
        assert!(updates[0].starts_with("Example — episode 7 airs at "));
        assert!(anime_updates(&[]).is_empty());
    }
}
