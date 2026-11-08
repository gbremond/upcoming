use crate::notifier::ReleaseNotification;
use crate::{anilist, bubble};
use chrono::NaiveDate;

pub(crate) fn print_section(lines: &[String], empty_message: &str) {
    if lines.is_empty() {
        println!("  {empty_message}");
    }
    for line in lines {
        println!("  {line}");
    }
}

pub(crate) fn print_reminders(reminders: &[ReleaseNotification]) {
    for reminder in reminders {
        println!(
            "    Reminder ({:?}): {}",
            reminder.reason,
            reminder.release.title.as_deref().unwrap_or("(untitled)")
        );
    }
}

pub(crate) fn album_overview(series: &bubble::BubbleSeries, today: NaiveDate) -> Vec<String> {
    series
        .upcoming_albums(today)
        .into_iter()
        .map(|(album, date)| {
            let days = (date - today).num_days();
            let timing = if days == 0 {
                "today".to_owned()
            } else {
                format!("in {days} days")
            };
            format!(
                "❗ {} | {} | {} | {timing}",
                date.format("%d/%m/%Y"),
                album.tome_label(),
                album.title.as_deref().unwrap_or("(untitled)")
            )
        })
        .collect()
}

pub(crate) fn anime_overview(seasons: &[anilist::Anime]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut announced = std::collections::HashSet::new();

    for season in seasons {
        if let Some(episode) = &season.next_airing_episode {
            let local_time = episode.airing_at.with_timezone(&chrono::Local);
            lines.push(format!(
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
            lines.push(format!(
                "❗ Announced sequel: {} ({}) — {}",
                sequel.title(),
                sequel.id,
                sequel.release_window()
            ));
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn album_overview_lists_upcoming_albums_by_date() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let series: bubble::BubbleSeries = serde_json::from_value(serde_json::json!({
            "albums": [
                {"objectId": "later", "title": "Later", "tome": 3,
                 "prints": [{"publicationDate": "2026-11-18T00:00:00Z"}]},
                {"objectId": "today", "title": null, "tome": null,
                 "prints": [{"publicationDate": "2026-11-08T00:00:00Z"}]},
                {"objectId": "past", "title": "Past", "tome": 1,
                 "prints": [{"publicationDate": "2026-01-01T00:00:00Z"}]}
            ]
        }))
        .unwrap();

        assert_eq!(
            album_overview(&series, today),
            [
                "❗ 08/11/2026 | Unnumbered | (untitled) | today",
                "❗ 18/11/2026 | T03 | Later | in 10 days",
            ]
        );
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
            anime_overview(&seasons),
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

        let lines = anime_overview(&seasons);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Example — episode 7 airs at "));
        assert!(anime_overview(&[]).is_empty());
    }
}
