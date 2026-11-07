mod anilist;
use anilist::AniListClient;
mod bubble;
use bubble::BubbleClient;
mod config;
use config::{Config, parse_config};
mod notifier;
use notifier::{DiscordClient, send_all};
mod reminder;
mod state;
use reminder::{Reminder, reminder_for};
use std::error::Error;
use std::fs;

fn main() -> Result<(), Box<dyn Error>> {
    let contents = fs::read_to_string("subscriptions.yaml")?;
    let config: Config = parse_config(&contents)?;

    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let bubble_client = BubbleClient::new(http.clone(), "https://api.bubblebd.com/v1.6".to_owned());
    let anilist_client =
        anilist::AniListClient::new(http.clone(), "https://graphql.anilist.co".to_owned());

    let webhook_url = std::env::var("DISCORD_WEBHOOK_URL").map_err(|error| {
        std::io::Error::other(format!("Could not read DISCORD_WEBHOOK_URL: {error}"))
    })?;
    let notifier = DiscordClient::new(http, webhook_url);

    let state_path = std::path::Path::new("state.json");
    let mut state = state::TrackerState::load(state_path)?;

    let today = chrono::Local::now().date_naive();
    let mut messages = Vec::new();

    for subscription in &config.subscriptions {
        match subscription.provider.as_str() {
            "bubble" => process_subscription(
                subscription,
                &bubble_client,
                &mut state,
                today,
                &mut messages,
            )?,
            "anilist" => show_anime(subscription, &anilist_client)?,
            provider => {
                return Err(format!("Unsupported provider: {provider}").into());
            }
        }
    }

    notify_and_save(&notifier, &messages, &state, state_path)?;

    Ok(())
}

fn process_subscription(
    subscription: &config::Subscription,
    bubble_client: &BubbleClient,
    state: &mut state::TrackerState,
    today: chrono::NaiveDate,
    messages: &mut Vec<String>,
) -> Result<(), Box<dyn Error>> {
    println!(
        "=== {}: {} ({}) ===",
        subscription.provider, subscription.name, subscription.id
    );

    if subscription.provider != "bubble" {
        return Err(format!("Unsupported provider: {}", subscription.provider).into());
    }

    let series = bubble_client.fetch_bubble_series(&subscription.id)?;

    match state.update_series(&subscription.provider, &subscription.id, series.snapshot()) {
        Some(changes) => {
            for change in changes {
                if change.should_notify(today) {
                    println!("  Relevant change: {change:?}");
                    messages.push(format!("{}: {change:?}", subscription.name));
                }
            }
        }
        None => println!("  First run for this subscription: saving baseline."),
    }

    show_upcoming(&series, &subscription.name, today, messages);

    Ok(())
}

fn show_anime(
    subscription: &config::Subscription,
    client: &anilist::AniListClient,
) -> Result<(), Box<dyn Error>> {
    let id = subscription.id.parse::<i32>().map_err(|error| {
        std::io::Error::other(format!(
            "Invalid AniList ID for {}: {error}",
            subscription.name
        ))
    })?;

    if id <= 0 {
        return Err(std::io::Error::other("AniList ID must be positive").into());
    }

    let seasons = client.fetch_sequel_chain(id)?;

    println!("=== anilist: {} ({id}) ===", subscription.name);

    let updates = anime_updates(&seasons);
    if updates.is_empty() {
        println!("  No scheduled episodes or announced sequels.");
    } else {
        for update in updates {
            println!("  {update}");
        }
    }

    Ok(())
}

fn anime_updates(seasons: &[anilist::Anime]) -> Vec<String> {
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

fn show_upcoming(
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

fn notify_and_save(
    notifier: &dyn notifier::Notifier,
    messages: &[String],
    state: &state::TrackerState,
    path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    if let Err(failures) = send_all(notifier, messages) {
        for (index, error) in &failures {
            eprintln!("Notification {} failed: {error}", index + 1);
        }

        return Err(std::io::Error::other("notification delivery failed").into());
    }

    state.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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

    struct FakeNotifier {
        fail: bool,
    }

    impl notifier::Notifier for FakeNotifier {
        fn send(&self, _message: &str) -> Result<(), Box<dyn Error>> {
            if self.fail {
                return Err(std::io::Error::other("delivery failed").into());
            }
            Ok(())
        }
    }

    #[test]
    fn saves_state_only_after_successful_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let previous = state::TrackerState::default();
        previous.save(&path).unwrap();

        let mut updated = state::TrackerState::default();
        updated
            .providers
            .insert("bubble".to_owned(), BTreeMap::new());
        let messages = vec!["New release".to_owned()];

        let failing = FakeNotifier { fail: true };
        assert!(notify_and_save(&failing, &messages, &updated, &path).is_err());
        assert_eq!(state::TrackerState::load(&path).unwrap(), previous);

        let successful = FakeNotifier { fail: false };
        notify_and_save(&successful, &messages, &updated, &path).unwrap();
        assert_eq!(state::TrackerState::load(&path).unwrap(), updated);
    }
}
