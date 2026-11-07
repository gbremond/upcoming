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
    let webhook_url = std::env::var("DISCORD_WEBHOOK_URL").map_err(|error| {
        std::io::Error::other(format!("Could not read DISCORD_WEBHOOK_URL: {error}"))
    })?;
    let notifier = DiscordClient::new(http, webhook_url);

    let state_path = std::path::Path::new("state.json");
    let mut state = state::TrackerState::load(state_path)?;

    let today = chrono::Local::now().date_naive();
    let mut messages = Vec::new();

    for subscription in config.subscriptions {
        println!(
            "=== {}: {} ({}) ===",
            subscription.provider, subscription.name, subscription.id
        );

        if subscription.provider != "bubble" {
            return Err(format!("Unsupported provider: {}", subscription.provider).into());
        }

        let series = bubble_client.fetch_bubble_series(&subscription.id)?;

        let snapshot = series.snapshot();

        match state.update_series(&subscription.provider, &subscription.id, snapshot) {
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

        let upcoming = series.upcoming_albums(today);

        if upcoming.is_empty() {
            println!("No upcoming albums with known publication dates.");
        } else {
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
                    "  {} | {tome} | {title} | {timing}",
                    date.format("%d/%m/%Y")
                );

                if let Some(reminder) = reminder_for(date, today) {
                    let description = match reminder {
                        Reminder::Today => "releases today",
                        Reminder::InOneWeek => "releases in one week",
                    };

                    let message = format!(
                        "{} — {tome} — {title} {description} ({date})",
                        subscription.name
                    );
                    println!("    Reminder: {message}");
                    messages.push(message);
                }
            }
        }
    }

    if let Err(failures) = send_all(&notifier, &messages) {
        for (index, error) in &failures {
            eprintln!("Notification {} failed: {error}", index + 1);
        }

        return Err(std::io::Error::other("notification delivery failed").into());
    }

    state.save(state_path)?;

    Ok(())
}
