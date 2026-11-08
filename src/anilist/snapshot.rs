use super::model::{Anime, ScheduledEpisode};
use crate::release_date::ReleaseDate;
use crate::state::{ReleaseRecord, SeriesSnapshot};
use anyhow::{Context, Result};

pub(crate) fn anime_snapshot(seasons: &[Anime]) -> Result<SeriesSnapshot> {
    let mut releases = std::collections::BTreeMap::new();

    for season in seasons {
        releases.insert(
            format!("season:{}", season.id),
            season.release_record().with_context(|| {
                format!(
                    "Failed to map release record for anime {} ({})",
                    season.id,
                    season.title()
                )
            })?,
        );

        for episode in &season.scheduled_episodes {
            releases.insert(
                format!("episode:{}", episode.id),
                episode.release_record(season),
            );
        }
    }

    Ok(SeriesSnapshot { releases })
}

impl Anime {
    fn release_record(&self) -> Result<ReleaseRecord> {
        let date = match &self.start_date {
            Some(date) => date.to_release_date()?,
            None => None,
        };

        let title = self
            .title
            .english
            .as_ref()
            .or(self.title.romaji.as_ref())
            .cloned();

        Ok(ReleaseRecord {
            date,
            title,
            image_url: self
                .cover_image
                .as_ref()
                .and_then(|cover| cover.extra_large.clone()),
            url: Some(format!("https://anilist.co/anime/{}", self.id)),
        })
    }
}

impl ScheduledEpisode {
    fn release_record(&self, season: &Anime) -> ReleaseRecord {
        ReleaseRecord {
            date: Some(ReleaseDate::Instant(self.airing_at)),
            title: Some(format!("{} — Episode {}", season.title(), self.episode)),
            image_url: season
                .cover_image
                .as_ref()
                .and_then(|cover| cover.extra_large.clone()),
            url: Some(format!("https://anilist.co/anime/{}", season.id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anilist::test_support::{FRIEREN_SEASON_2, FRIEREN_SEASON_3_ANNOUNCED, parse_anime};
    use pretty_assertions::assert_eq;

    #[test]
    fn season_record_preserves_an_announced_month() {
        let anime = parse_anime(FRIEREN_SEASON_3_ANNOUNCED).unwrap();

        let record = anime.release_record().unwrap();

        assert_eq!(
            record.date,
            Some(ReleaseDate::Partial {
                year: Some(2027),
                month: Some(10),
                day: None,
            })
        );
        assert_eq!(
            record.title.as_deref(),
            Some("Sousou no Frieren 3rd Season")
        );
        assert_eq!(
            record.url.as_deref(),
            Some("https://anilist.co/anime/209939")
        );
        assert!(record.image_url.is_none());
    }

    #[test]
    fn snapshot_contains_each_season() {
        let seasons = [
            parse_anime(FRIEREN_SEASON_2).unwrap(),
            parse_anime(FRIEREN_SEASON_3_ANNOUNCED).unwrap(),
        ];

        let snapshot = anime_snapshot(&seasons).unwrap();

        assert_eq!(snapshot.releases.len(), 2);
        assert_eq!(
            snapshot.releases["season:182255"].date,
            Some(ReleaseDate::Date(
                chrono::NaiveDate::from_ymd_opt(2026, 1, 16).unwrap()
            ))
        );
        assert_eq!(
            snapshot.releases["season:209939"].date,
            Some(ReleaseDate::Partial {
                year: Some(2027),
                month: Some(10),
                day: None,
            })
        );
    }

    #[test]
    fn snapshot_contains_episode_metadata() {
        let mut anime = parse_anime(FRIEREN_SEASON_2).unwrap();
        let airing_at = "2027-10-08T14:00:00Z".parse().unwrap();
        anime.scheduled_episodes.push(ScheduledEpisode {
            id: 1001,
            episode: 7,
            airing_at,
        });

        let snapshot = anime_snapshot(&[anime]).unwrap();
        let record = &snapshot.releases["episode:1001"];

        assert_eq!(snapshot.releases.len(), 2);
        assert_eq!(record.date, Some(ReleaseDate::Instant(airing_at)));
        assert_eq!(
            record.title.as_deref(),
            Some("Sousou no Frieren 2nd Season — Episode 7")
        );
        assert_eq!(
            record.image_url.as_deref(),
            Some("https://s4.anilist.co/file/anilistcdn/media/anime/cover/large/bx182255.jpg")
        );
        assert_eq!(
            record.url.as_deref(),
            Some("https://anilist.co/anime/182255")
        );
    }
}
