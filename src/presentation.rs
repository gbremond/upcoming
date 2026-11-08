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
    messages: &mut Vec<String>,
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
            messages.push(message);
        }
    }
}

pub(crate) fn episode_reminders(
    snapshot: &crate::state::SeriesSnapshot,
    today: chrono::NaiveDate,
) -> Vec<String> {
    let mut messages = Vec::new();

    for (id, record) in &snapshot.releases {
        if !id.starts_with("episode:") {
            continue;
        }

        let Some(date) = record.date.as_ref().and_then(|date| date.reminder_date()) else {
            continue;
        };

        let description = match reminder_for(date, today) {
            Some(Reminder::Today) => "airs today",
            Some(Reminder::InOneWeek) => "airs in one week",
            None => continue,
        };

        let title = record.title.as_deref().unwrap_or("(untitled episode)");
        let mut message = format!("{title} {description} ({date})");

        if let Some(url) = &record.url {
            message.push_str(&format!("\n{url}"));
        }

        messages.push(message);
    }

    messages
}

#[cfg(test)]
mod tests {
    use super::*;

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
