mod anilist;
mod bubble;
use bubble::BubbleClient;
mod config;
use config::{Config, parse_config};
mod notifier;
use notifier::DiscordClient;
mod presentation;
mod release_date;
mod reminder;
mod state;
use anyhow::{Context, Result};
use std::fs;
mod tracker;
use tracker::{notify_and_save, process_anilist_subscription, process_bubble_subscription};

fn main() -> Result<()> {
    let contents =
        fs::read_to_string("subscriptions.yaml").context("Failed to read subscriptions.yaml")?;
    let config: Config = parse_config(&contents).context("Failed to parse subscriptions.yaml")?;

    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client")?;
    let bubble_client = BubbleClient::new(http.clone(), "https://api.bubblebd.com/v1.6".to_owned());
    let anilist_client =
        anilist::AniListClient::new(http.clone(), "https://graphql.anilist.co".to_owned());

    let webhook_url =
        std::env::var("DISCORD_WEBHOOK_URL").unwrap_or_else(|_| "FAKE_KEY".to_owned());
    let notifier = DiscordClient::new(http, webhook_url);

    let state_path = std::path::Path::new("state.json");
    let mut state = state::TrackerState::load(state_path)?;

    let today = chrono::Local::now().date_naive();
    let mut messages = Vec::new();

    for subscription in &config.subscriptions {
        match subscription.provider.as_str() {
            "bubble" => process_bubble_subscription(
                subscription,
                &bubble_client,
                &mut state,
                today,
                &mut messages,
            )?,
            "anilist" => process_anilist_subscription(
                subscription,
                &anilist_client,
                &mut state,
                today,
                &mut messages,
            )?,
            provider => {
                anyhow::bail!(
                    "Unsupported provider '{provider}' for subscription '{}'",
                    subscription.name
                );
            }
        }
    }

    notify_and_save(&notifier, &messages, &state, state_path)?;

    Ok(())
}
