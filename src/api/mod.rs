mod releases;
pub(crate) mod status;
mod subscriptions;

use self::status::SharedStatus;
use crate::config::Subscription;
use axum::Router;
use axum::extract::FromRef;
use axum::routing::get;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status::get_status))
        .route("/subscriptions", get(subscriptions::get_subscriptions))
        .route("/releases", get(releases::get_releases))
        .with_state(state)
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) status: SharedStatus,
    pub(crate) subscriptions: Arc<Vec<Subscription>>,
    pub(crate) state_path: PathBuf,
}

impl FromRef<AppState> for SharedStatus {
    fn from_ref(app: &AppState) -> Self {
        app.status.clone()
    }
}

async fn health() -> &'static str {
    "ok"
}
