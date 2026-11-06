mod bubble;
use bubble::fetch_bubble_series;
mod config;
use config::{Config, parse_config};
use std::error::Error;
use std::fs;

fn main() -> Result<(), Box<dyn Error>> {
    let contents = fs::read_to_string("subscriptions.yaml")?;
    let config: Config = parse_config(&contents)?;

    for subscription in config.subscriptions {
        println!(
            "{}: {} ({})",
            subscription.provider, subscription.name, subscription.id
        );

        if subscription.provider != "bubble" {
            return Err(format!("Unsupported provider: {}", subscription.provider).into());
        }

        let response = fetch_bubble_series(&subscription.id)?;
        println!("{response:?}");
    }

    Ok(())
}
