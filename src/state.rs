use crate::release_date::ReleaseDate;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrackerState {
    pub(crate) providers: BTreeMap<String, BTreeMap<String, SeriesSnapshot>>,
}

impl TrackerState {
    pub(crate) fn load(path: &std::path::Path) -> Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to read state {}", path.display()));
            }
        };

        serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse state {}", path.display()))
    }

    pub(crate) fn save(&self, path: &std::path::Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));

        let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "Failed to create temporary state file in {}",
                parent.display()
            )
        })?;
        serde_json::to_writer_pretty(&mut temporary, self)
            .with_context(|| format!("Failed to serialize state for {}", path.display()))?;
        temporary
            .as_file()
            .sync_all()
            .with_context(|| format!("Failed to sync temporary state for {}", path.display()))?;
        temporary
            .persist(path)
            .with_context(|| format!("Failed to persist state to {}", path.display()))?;

        Ok(())
    }

    /// Stores the latest snapshot of a series and returns what changed since
    /// the previous one, or `None` on the first run (nothing to compare with).
    pub(crate) fn record_snapshot(
        &mut self,
        provider: &str,
        series_id: &str,
        snapshot: SeriesSnapshot,
    ) -> Option<Vec<ReleaseChange>> {
        let series = self.providers.entry(provider.to_owned()).or_default();

        let changes = series
            .get(series_id)
            .map(|previous| snapshot.changes_since(previous));

        series.insert(series_id.to_owned(), snapshot);
        changes
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SeriesSnapshot {
    pub(crate) releases: BTreeMap<String, ReleaseRecord>,
}

impl SeriesSnapshot {
    pub(crate) fn changes_since(&self, previous: &SeriesSnapshot) -> Vec<ReleaseChange> {
        let mut changes = Vec::new();

        for (id, current) in &self.releases {
            match previous.releases.get(id) {
                None => changes.push(ReleaseChange::Added {
                    id: id.clone(),
                    date: current.date.clone(),
                }),
                Some(old) if old.date != current.date => {
                    changes.push(ReleaseChange::DateChanged {
                        id: id.clone(),
                        old_date: old.date.clone(),
                        new_date: current.date.clone(),
                    });
                }
                Some(_) => {}
            }
        }

        changes
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum ReleaseChange {
    Added {
        id: String,
        date: Option<ReleaseDate>,
    },
    DateChanged {
        id: String,
        old_date: Option<ReleaseDate>,
        new_date: Option<ReleaseDate>,
    },
}

impl ReleaseChange {
    pub(crate) fn should_notify(&self, today: NaiveDate) -> bool {
        fn relevant(date: Option<&ReleaseDate>, today: NaiveDate) -> bool {
            match date.and_then(ReleaseDate::reminder_date) {
                Some(date) => date >= today,
                None => true,
            }
        }

        match self {
            ReleaseChange::Added { date, .. } => relevant(date.as_ref(), today),
            ReleaseChange::DateChanged {
                old_date, new_date, ..
            } => relevant(old_date.as_ref(), today) || relevant(new_date.as_ref(), today),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ReleaseRecord {
    pub(crate) date: Option<ReleaseDate>,
    pub(crate) title: Option<String>,
    pub(crate) image_url: Option<String>,
    pub(crate) url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_state_retains_file_context_and_json_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "{invalid").unwrap();

        let error = TrackerState::load(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("Failed to parse state {}", path.display()))
        );
        assert!(error.downcast_ref::<serde_json::Error>().is_some());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{invalid");
    }

    #[test]
    fn tracker_state_round_trips_multiple_subscriptions() {
        let mut state = TrackerState::default();

        let bubble = state.providers.entry("bubble".to_owned()).or_default();

        bubble.insert(
            "series-1".to_owned(),
            SeriesSnapshot {
                releases: BTreeMap::from([("album-1".to_owned(), record(None))]),
            },
        );
        bubble.insert(
            "series-2".to_owned(),
            SeriesSnapshot {
                releases: BTreeMap::new(),
            },
        );

        let json = serde_json::to_string(&state).unwrap();
        let restored: TrackerState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored, state);
    }

    #[test]
    fn missing_state_file_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let state = TrackerState::load(&path).unwrap();

        assert_eq!(state, TrackerState::default());
    }

    #[test]
    fn malformed_state_file_returns_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "not json").unwrap();

        assert!(TrackerState::load(&path).is_err());
    }

    #[test]
    fn existing_state_file_is_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"providers":{"bubble":{}}}"#).unwrap();

        let state = TrackerState::load(&path).unwrap();

        assert!(state.providers.contains_key("bubble"));
    }

    #[test]
    fn saves_and_replaces_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let first = TrackerState::default();
        first.save(&path).unwrap();
        assert_eq!(TrackerState::load(&path).unwrap(), first);

        let mut second = TrackerState::default();
        second
            .providers
            .insert("bubble".to_owned(), BTreeMap::new());

        second.save(&path).unwrap();
        assert_eq!(TrackerState::load(&path).unwrap(), second);
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let snapshot = SeriesSnapshot {
            releases: BTreeMap::from([
                (
                    "album-18".to_owned(),
                    record(Some(NaiveDate::from_ymd_opt(2026, 10, 8).unwrap())),
                ),
                ("album-19".to_owned(), record(None)),
            ]),
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: SeriesSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(restored, snapshot);
    }

    #[test]
    fn detects_added_albums_and_date_changes() {
        let date = NaiveDate::from_ymd_opt(2027, 5, 21).unwrap();

        let previous = SeriesSnapshot {
            releases: BTreeMap::from([
                ("unchanged".to_owned(), record(Some(date))),
                ("announced".to_owned(), record(None)),
            ]),
        };

        let current = SeriesSnapshot {
            releases: BTreeMap::from([
                ("unchanged".to_owned(), record(Some(date))),
                ("announced".to_owned(), record(Some(date))),
                ("new".to_owned(), record(None)),
            ]),
        };

        assert_eq!(
            current.changes_since(&previous),
            vec![
                ReleaseChange::DateChanged {
                    id: "announced".to_owned(),
                    old_date: None,
                    new_date: Some(ReleaseDate::Date(date)),
                },
                ReleaseChange::Added {
                    id: "new".to_owned(),
                    date: None,
                },
            ]
        );
    }

    #[test]
    fn first_observation_is_a_baseline_then_changes_are_reported() {
        let mut state = TrackerState::default();

        let first = SeriesSnapshot {
            releases: BTreeMap::from([("album-1".to_owned(), record(None))]),
        };

        assert_eq!(state.record_snapshot("bubble", "series-1", first), None);

        let date = NaiveDate::from_ymd_opt(2027, 5, 21).unwrap();
        let second = SeriesSnapshot {
            releases: BTreeMap::from([("album-1".to_owned(), record(Some(date)))]),
        };

        assert_eq!(
            state.record_snapshot("bubble", "series-1", second),
            Some(vec![ReleaseChange::DateChanged {
                id: "album-1".to_owned(),
                old_date: None,
                new_date: Some(ReleaseDate::Date(date)),
            }])
        );
    }

    #[test]
    fn detects_changes_after_reloading_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let date = NaiveDate::from_ymd_opt(2027, 5, 21).unwrap();

        let mut state = TrackerState::default();
        let baseline = SeriesSnapshot {
            releases: BTreeMap::from([("album-1".to_owned(), record(None))]),
        };

        assert_eq!(state.record_snapshot("bubble", "series-1", baseline), None);
        state.save(&path).unwrap();

        // Simulate a new program run.
        let mut reloaded = TrackerState::load(&path).unwrap();
        let updated = SeriesSnapshot {
            releases: BTreeMap::from([("album-1".to_owned(), record(Some(date)))]),
        };

        assert_eq!(
            reloaded.record_snapshot("bubble", "series-1", updated),
            Some(vec![ReleaseChange::DateChanged {
                id: "album-1".to_owned(),
                old_date: None,
                new_date: Some(ReleaseDate::Date(date)),
            }])
        );
    }

    #[test]
    fn notifies_about_upcoming_or_unknown_releases() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let past = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();

        let added = |date| ReleaseChange::Added {
            id: "album-1".to_owned(),
            date,
        };

        assert!(added(Some(ReleaseDate::Date(today))).should_notify(today));
        assert!(added(None).should_notify(today));
        assert!(!added(Some(ReleaseDate::Date(past))).should_notify(today));

        let moved_to_past = ReleaseChange::DateChanged {
            id: "album-1".to_owned(),
            old_date: Some(ReleaseDate::Date(today)),
            new_date: Some(ReleaseDate::Date(past)),
        };

        assert!(moved_to_past.should_notify(today));
    }

    fn record(date: Option<NaiveDate>) -> ReleaseRecord {
        ReleaseRecord {
            date: date.map(ReleaseDate::Date),
            ..ReleaseRecord::default()
        }
    }

    #[test]
    fn metadata_changes_do_not_trigger_release_alerts() {
        let previous = SeriesSnapshot {
            releases: BTreeMap::from([(
                "album-1".to_owned(),
                ReleaseRecord {
                    title: Some("Old title".to_owned()),
                    ..ReleaseRecord::default()
                },
            )]),
        };

        let current = SeriesSnapshot {
            releases: BTreeMap::from([(
                "album-1".to_owned(),
                ReleaseRecord {
                    title: Some("Corrected title".to_owned()),
                    image_url: Some("https://example.com/cover.jpg".to_owned()),
                    ..ReleaseRecord::default()
                },
            )]),
        };

        assert!(current.changes_since(&previous).is_empty());
    }
}
