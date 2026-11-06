mod bubble;
use bubble::BubbleClient;
mod config;
use config::{Config, parse_config};
mod state;
use std::error::Error;
use std::fs;

fn main() -> Result<(), Box<dyn Error>> {
    let contents = fs::read_to_string("subscriptions.yaml")?;
    let config: Config = parse_config(&contents)?;
    let bubble_client = BubbleClient::new(
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?,
        "https://api.bubblebd.com/v1.6".to_string(),
    );

    let today = chrono::Local::now().date_naive();

    for subscription in config.subscriptions {
        println!(
            "=== {}: {} ({}) ===",
            subscription.provider, subscription.name, subscription.id
        );

        if subscription.provider != "bubble" {
            return Err(format!("Unsupported provider: {}", subscription.provider).into());
        }

        let series = bubble_client.fetch_bubble_series(&subscription.id)?;

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
            }
        }
    }

    Ok(())
}
