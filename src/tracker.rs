use crate::anilist::AniListClient;
use crate::bubble::BubbleClient;
use crate::config;
use crate::notifier::{self, send_all};
use crate::presentation::{anime_updates, show_upcoming};
use crate::state;
use std::error::Error;

pub(crate) fn process_bubble_subscription(
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

pub(crate) fn process_anilist_subscription(
    subscription: &config::Subscription,
    client: &AniListClient,
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

pub(crate) fn notify_and_save(
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
