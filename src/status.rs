use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Local};
use serde::Serialize;
use std::sync::{Arc, RwLock};

pub(crate) async fn get_status(State(status): State<SharedStatus>) -> Json<Status> {
    Json(status.read().unwrap().clone())
}

pub(crate) type SharedStatus = Arc<RwLock<Status>>;

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Status {
    running: bool,
    last_started: Option<DateTime<Local>>,
    last_finished: Option<DateTime<Local>>,
    last_error: Option<String>,
    next_run: Option<DateTime<Local>>,
}

impl Status {
    pub(crate) fn run_started(&mut self, at: DateTime<Local>) {
        self.running = true;
        self.last_started = Some(at);
    }
    pub(crate) fn run_finished(
        &mut self,
        at: DateTime<Local>,
        result: &anyhow::Result<()>,
        next_run: DateTime<Local>,
    ) {
        self.running = false;
        self.last_finished = Some(at);
        self.last_error = result.as_ref().err().map(|error| format!("{error:#}"));
        self.next_run = Some(next_run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use tower::ServiceExt;

    fn local(h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 8, h, min, 0).unwrap()
    }

    async fn get_json(status: SharedStatus) -> serde_json::Value {
        let app = axum::Router::new()
            .route("/status", axum::routing::get(get_status))
            .with_state(status);
        let response = app
            .oneshot(Request::get("/status").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn reports_a_run_in_progress() {
        let status = SharedStatus::default();
        status.write().unwrap().run_started(local(16, 0));

        let json = get_json(status).await;

        assert_eq!(json["running"], true);
        assert_eq!(json["last_started"], serde_json::json!(local(16, 0)));
        assert_eq!(json["last_finished"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn reports_the_error_and_next_run_of_a_failed_run() {
        let status = SharedStatus::default();
        status.write().unwrap().run_started(local(16, 0));
        status.write().unwrap().run_finished(
            local(16, 1),
            &Err(anyhow::anyhow!("AniList down")),
            local(23, 0),
        );

        let json = get_json(status).await;

        assert_eq!(json["running"], false);
        assert_eq!(json["last_finished"], serde_json::json!(local(16, 1)));
        assert_eq!(json["last_error"], "AniList down");
        assert_eq!(json["next_run"], serde_json::json!(local(23, 0)));
    }

    #[test]
    fn a_successful_run_clears_the_previous_error() {
        let mut status = Status::default();
        status.run_finished(local(16, 0), &Err(anyhow::anyhow!("boom")), local(23, 0));
        status.run_finished(local(16, 5), &Ok(()), local(23, 0));

        assert_eq!(status.last_error, None);
    }
}
