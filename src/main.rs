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
mod request_pacing;
#[cfg(test)]
mod scenario_tests;
mod state;
use anyhow::{Context, Result};
use std::{fs, time::Duration};
mod tracker;

#[tokio::main]
async fn main() -> Result<()> {
    let contents =
        fs::read_to_string("subscriptions.yaml").context("Failed to read subscriptions.yaml")?;
    let config: Config = parse_config(&contents).context("Failed to parse subscriptions.yaml")?;

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client")?;
    let bubble_client = BubbleClient::new(http.clone(), "https://api.bubblebd.com/v1.6".to_owned());
    let anilist_client = anilist::AniListClient::new(
        http.clone(),
        "https://graphql.anilist.co".to_owned(),
        Duration::from_secs(1),
    );

    let webhook_url =
        std::env::var("DISCORD_WEBHOOK_URL").unwrap_or_else(|_| "FAKE_KEY".to_owned());
    let notifier = DiscordClient::new(http, webhook_url);

    let today = chrono::Local::now().date_naive();
    tracker::run(
        &config.subscriptions,
        &bubble_client,
        &anilist_client,
        &notifier,
        std::path::Path::new("state.json"),
        today,
    )
    .await
}
