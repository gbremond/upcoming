use crate::anilist::AniListClient;
use crate::bubble::BubbleClient;
use crate::config::Subscription;
use crate::notifier::{NotificationReason, Notifier, ReleaseNotification};
use crate::release_date::ReleaseDate;
use crate::state::TrackerState;
use crate::tracker;
use anyhow::Result;
use chrono::{Datelike, NaiveDate};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TODAY: &str = "2026-11-08";

#[tokio::test]
async fn first_run_saves_a_baseline_without_notifying() {
    let mut world = World::new().await;
    world.follow_bubble(
        "les-5-terres",
        "Les 5 Terres",
        vec![album(1, "2026-01-15"), album(2, "2026-12-08")],
    );
    world.follow_anilist(
        "Jujutsu Kaisen",
        Season::new(1, "Jujutsu Kaisen").episode(1, "2026-11-20"),
    );

    assert!(world.run().await.is_empty());
    assert_eq!(
        world.saved_release_ids("bubble", "les-5-terres"),
        ["album-1", "album-2"]
    );
    assert_eq!(
        world.saved_release_ids("anilist", "1"),
        ["episode:1001", "season:1"]
    );

    assert!(
        world.run().await.is_empty(),
        "an identical second run has nothing new to report"
    );
}

#[tokio::test]
async fn new_episode_is_notified() {
    let mut world = World::new().await;
    world.follow_anilist(
        "Frieren",
        Season::new(1, "Frieren").episode(1, "2026-11-20"),
    );
    world.run().await;

    world.update_season(1, |season| season.episode(2, "2026-11-27"));

    assert_eq!(world.run().await, ["added: Frieren / Frieren — Episode 2"]);
}

#[tokio::test]
async fn new_season_is_notified() {
    let mut world = World::new().await;
    world.follow_anilist(
        "Jujutsu Kaisen",
        Season::new(1, "Jujutsu Kaisen").started("2020-10-03"),
    );
    world.run().await;

    // AniList links an announced sequel to season 1.
    world.update_season(1, |season| season.sequel(2));
    world.add_season(Season::new(2, "Jujutsu Kaisen Season 2").announced_for(2027));

    assert_eq!(
        world.run().await,
        ["added: Jujutsu Kaisen / Jujutsu Kaisen Season 2"]
    );
}

#[tokio::test]
async fn new_album_is_notified() {
    let mut world = World::new().await;
    world.follow_bubble("les-5-terres", "Les 5 Terres", vec![album(1, "2026-01-15")]);
    world.run().await;

    world.add_album("les-5-terres", album(2, "2027-02-10"));

    assert_eq!(world.run().await, ["added: Les 5 Terres / T02 — Tome 2"]);
}

#[tokio::test]
async fn changed_release_date_is_notified_with_the_previous_date() {
    let mut world = World::new().await;
    world.follow_bubble("les-5-terres", "Les 5 Terres", vec![album(2, "2026-12-08")]);
    world.run().await;

    world.move_album("les-5-terres", 2, "2027-01-12");

    assert_eq!(
        world.run().await,
        ["date changed from 2026-12-08 to 2027-01-12: Les 5 Terres / T02 — Tome 2"]
    );
}

#[tokio::test]
async fn reminders_are_sent_on_release_day_and_one_week_before() {
    let mut world = World::new().await;
    world.follow_bubble(
        "les-5-terres",
        "Les 5 Terres",
        vec![
            album(2, "2026-11-08"), // today
            album(3, "2026-11-11"), // in 3 days: no reminder
        ],
    );
    world.follow_anilist(
        "Frieren",
        Season::new(1, "Frieren").episode(5, "2026-11-15"), // in one week
    );

    // Reminders depend only on the date, so they fire even on a first run.
    assert_eq!(
        world.run().await,
        [
            "today: Les 5 Terres / T02 — Tome 2",
            "in one week: Frieren / Frieren — Episode 5",
        ]
    );
}

#[tokio::test]
async fn new_subscription_starts_with_its_own_silent_baseline() {
    let mut world = World::new().await;
    world.follow_bubble("les-5-terres", "Les 5 Terres", vec![album(1, "2026-01-15")]);
    world.run().await;

    world.add_album("les-5-terres", album(2, "2027-03-01"));
    world.follow_anilist(
        "Frieren",
        Season::new(1, "Frieren").episode(1, "2026-11-20"),
    );

    assert_eq!(
        world.run().await,
        ["added: Les 5 Terres / T02 — Tome 2"],
        "only the existing subscription reports changes"
    );
    assert_eq!(
        world.saved_release_ids("anilist", "1"),
        ["episode:1001", "season:1"]
    );
}

// ---------------------------------------------------------------------------
// World: the mocked APIs, the subscriptions and the state file of a scenario.
// ---------------------------------------------------------------------------

struct World {
    bubble_api: MockServer,
    anilist_api: MockServer,
    state_dir: tempfile::TempDir,
    subscriptions: Vec<Subscription>,
    bubble_series: BTreeMap<String, Vec<Album>>,
    seasons: BTreeMap<u32, Season>,
}

impl World {
    async fn new() -> Self {
        Self {
            bubble_api: MockServer::start().await,
            anilist_api: MockServer::start().await,
            state_dir: tempfile::tempdir().unwrap(),
            subscriptions: Vec::new(),
            bubble_series: BTreeMap::new(),
            seasons: BTreeMap::new(),
        }
    }

    fn follow_bubble(&mut self, series_id: &str, name: &str, albums: Vec<Album>) {
        self.subscribe("bubble", series_id, name);
        self.bubble_series.insert(series_id.to_owned(), albums);
    }

    fn follow_anilist(&mut self, name: &str, first_season: Season) {
        self.subscribe("anilist", &first_season.id.to_string(), name);
        self.add_season(first_season);
    }

    fn add_album(&mut self, series_id: &str, album: Album) {
        self.bubble_series.get_mut(series_id).unwrap().push(album);
    }

    fn move_album(&mut self, series_id: &str, tome: u32, new_release: &str) {
        let albums = self.bubble_series.get_mut(series_id).unwrap();
        let album = albums.iter_mut().find(|album| album.tome == tome).unwrap();
        album.release = date(new_release);
    }

    fn add_season(&mut self, season: Season) {
        self.seasons.insert(season.id, season);
    }

    fn update_season(&mut self, id: u32, change: impl FnOnce(Season) -> Season) {
        let season = self.seasons.remove(&id).unwrap();
        self.seasons.insert(id, change(season));
    }

    /// Runs the tracker once and returns the notifications it sent.
    async fn run(&self) -> Vec<String> {
        self.serve_current_api_responses().await;

        let http = reqwest::Client::new();
        let notifier = RecordingNotifier::default();
        tracker::run(
            &self.subscriptions,
            &BubbleClient::new(http.clone(), self.bubble_api.uri()),
            &AniListClient::new(http, self.anilist_api.uri(), Duration::ZERO),
            &notifier,
            &self.state_path(),
            date(TODAY),
        )
        .await
        .expect("tracker run failed");

        notifier.sent.into_inner().unwrap()
    }

    fn saved_release_ids(&self, provider: &str, series_id: &str) -> Vec<String> {
        let state = TrackerState::load(&self.state_path()).unwrap();
        state.providers[provider][series_id]
            .releases
            .keys()
            .cloned()
            .collect()
    }

    fn subscribe(&mut self, provider: &str, id: &str, name: &str) {
        self.subscriptions.push(Subscription {
            provider: provider.to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
        });
    }

    fn state_path(&self) -> std::path::PathBuf {
        self.state_dir.path().join("state.json")
    }

    async fn serve_current_api_responses(&self) {
        self.bubble_api.reset().await;
        for (series_id, albums) in &self.bubble_series {
            Mock::given(method("GET"))
                .and(path(format!("/series/{series_id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(bubble_json(albums)))
                .mount(&self.bubble_api)
                .await;
        }

        self.anilist_api.reset().await;
        for season in self.seasons.values() {
            Mock::given(method("POST"))
                .and(body_partial_json(
                    json!({ "variables": { "id": season.id } }),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(season.anilist_json()))
                .mount(&self.anilist_api)
                .await;
        }
    }
}

#[derive(Default)]
struct RecordingNotifier {
    sent: Mutex<Vec<String>>,
}

impl Notifier for RecordingNotifier {
    async fn send(&self, notification: &ReleaseNotification) -> Result<()> {
        self.sent.lock().unwrap().push(describe(notification));
        Ok(())
    }
}

fn describe(notification: &ReleaseNotification) -> String {
    let reason = match &notification.reason {
        NotificationReason::Added => "added".to_owned(),
        NotificationReason::DateChanged { old_date } => format!(
            "date changed from {} to {}",
            show(old_date),
            show(&notification.release.date)
        ),
        NotificationReason::Today => "today".to_owned(),
        NotificationReason::InOneWeek => "in one week".to_owned(),
    };
    let title = notification
        .release
        .title
        .as_deref()
        .unwrap_or("(untitled)");

    format!("{reason}: {} / {title}", notification.subscription_name)
}

fn show(date: &Option<ReleaseDate>) -> String {
    match date {
        Some(ReleaseDate::Date(date)) => date.to_string(),
        Some(ReleaseDate::Instant(instant)) => instant.date_naive().to_string(),
        Some(ReleaseDate::Partial { year, month, .. }) => format!("{year:?}-{month:?}"),
        None => "unknown".to_owned(),
    }
}

fn date(value: &str) -> NaiveDate {
    value.parse().unwrap()
}

// ---------------------------------------------------------------------------
// Fixtures: just enough of each API response for the tracker.
// ---------------------------------------------------------------------------

struct Album {
    tome: u32,
    release: NaiveDate,
}

/// Album "T{tome} — Tome {tome}", with Bubble ID `album-{tome}`.
fn album(tome: u32, release: &str) -> Album {
    Album {
        tome,
        release: date(release),
    }
}

fn bubble_json(albums: &[Album]) -> Value {
    let albums: Vec<Value> = albums
        .iter()
        .map(|album| {
            json!({
                "objectId": format!("album-{}", album.tome),
                "title": format!("Tome {}", album.tome),
                "tome": album.tome,
                "prints": [{ "publicationDate": format!("{}T00:00:00.000Z", album.release) }],
            })
        })
        .collect();

    json!({ "albums": albums })
}

struct Season {
    id: u32,
    title: &'static str,
    start_date: Value,
    episodes: Vec<(u32, NaiveDate)>,
    sequels: Vec<u32>,
}

impl Season {
    fn new(id: u32, title: &'static str) -> Self {
        Self {
            id,
            title,
            start_date: Value::Null,
            episodes: Vec::new(),
            sequels: Vec::new(),
        }
    }

    fn started(mut self, start: &str) -> Self {
        let start = date(start);
        self.start_date =
            json!({ "year": start.year(), "month": start.month(), "day": start.day() });
        self
    }

    fn announced_for(mut self, year: i32) -> Self {
        self.start_date = json!({ "year": year, "month": null, "day": null });
        self
    }

    /// Episode airing at noon UTC on `airs`, with AniList ID `{season}{number:03}`.
    fn episode(mut self, number: u32, airs: &str) -> Self {
        self.episodes.push((number, date(airs)));
        self
    }

    fn sequel(mut self, id: u32) -> Self {
        self.sequels.push(id);
        self
    }

    fn anilist_json(&self) -> Value {
        let episodes: Vec<Value> = self
            .episodes
            .iter()
            .map(|(number, airs)| {
                json!({
                    "id": self.id * 1000 + number,
                    "episode": number,
                    "airingAt": airs.and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp(),
                })
            })
            .collect();
        let sequels: Vec<Value> = self
            .sequels
            .iter()
            .map(|id| {
                json!({
                    "relationType": "SEQUEL",
                    "node": { "id": id, "type": "ANIME", "title": {}, "status": "NOT_YET_RELEASED" },
                })
            })
            .collect();

        json!({ "data": { "Media": {
            "id": self.id,
            "title": { "english": self.title },
            "startDate": self.start_date,
            "airingSchedule": { "pageInfo": { "hasNextPage": false }, "nodes": episodes },
            "relations": { "edges": sequels },
        }}})
    }
}
