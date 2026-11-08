mod anilist;
mod api;
use api::status;
mod bubble;
use bubble::BubbleClient;
mod config;
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
use std::{fs, path::PathBuf, sync::Arc, time::Duration};
mod schedule;
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

    let state_path = PathBuf::from("state.json");
    let subscriptions = Arc::new(config.subscriptions);
    let status = status::SharedStatus::default();
    let task_status = status.clone();
    let task_subscriptions = subscriptions.clone();
    let task_state_path = state_path.clone();

    let shutdown = tokio_util::sync::CancellationToken::new();

    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            shutdown_signal().await;
            shutdown.cancel();
        }
    });

    let task_shutdown = shutdown.clone();
    let tracker = tokio::spawn(async move {
        loop {
            let today = chrono::Local::now();
            task_status.write().unwrap().run_started(today);

            let result = tracker::run(
                &task_subscriptions,
                &bubble_client,
                &anilist_client,
                &notifier,
                &task_state_path,
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
            tokio::select! {
                _ = tokio::time::sleep((next - after_run).to_std().unwrap_or_default()) => {}
                _ = task_shutdown.cancelled() => break,
            }
        }
    });

    let app = api::router(api::AppState {
        status,
        subscriptions,
        state_path,
    });
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;

    tracker.await?; // wait for the current run to finish
    tracing::info!("bye");

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl+C");
    };
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown requested");
}
