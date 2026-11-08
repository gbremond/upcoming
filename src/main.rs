mod anilist;
mod bubble;
use bubble::BubbleClient;
mod config;
mod console;
use config::{Config, parse_config};
mod notifier;
use notifier::DiscordClient;
mod release_date;
mod reminder;
mod request_pacing;
#[cfg(test)]
mod scenario_tests;
mod state;
use anyhow::{Context, Result};
use std::{fs, time::Duration};
mod schedule;
mod status;
mod tracker;

#[tokio::main]
async fn main() -> Result<()> {
    // Logs go to stderr; RUST_LOG overrides the default level (e.g. RUST_LOG=upcoming=info).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "upcoming=debug".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let subscription_config =
        fs::read_to_string("subscriptions.yaml").context("Failed to read subscriptions.yaml")?;
    let config: Config =
        parse_config(&subscription_config).context("Failed to parse subscriptions.yaml")?;

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

    let status = status::SharedStatus::default();
    let task_status = status.clone();

    tokio::spawn(async move {
        loop {
            let today = chrono::Local::now();
            task_status.write().unwrap().run_started(today);

            let result = tracker::run(
                &config.subscriptions,
                &bubble_client,
                &anilist_client,
                &notifier,
                std::path::Path::new("state.json"),
                today.date_naive(),
            )
            .await;
            if let Err(error) = &result {
                tracing::error!("Error running tracker: {error:#}");
            }

            let after_run = chrono::Local::now();
            let next = schedule::next_run(
                chrono::NaiveTime::from_hms_opt(16, 0, 0).unwrap(),
                &after_run,
            );
            task_status
                .write()
                .unwrap()
                .run_finished(after_run, &result, next);
            tracing::info!(next_run = %next.format("%d/%m/%Y %H:%M"), "tracker idle");
            tokio::time::sleep((next - after_run).to_std().unwrap_or_default()).await;
        }
    });

    let app = axum::Router::new()
        .route("/health", axum::routing::get(health))
        .route("/status", axum::routing::get(status::get_status))
        .with_state(status);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}
