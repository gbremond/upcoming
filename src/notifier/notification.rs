use crate::release_date::ReleaseDate;
use crate::state::ReleaseRecord;

#[derive(Debug)]
pub(crate) struct ReleaseNotification {
    pub(crate) subscription_name: String,
    pub(crate) release: ReleaseRecord,
    pub(crate) reason: NotificationReason,
}

#[derive(Debug)]
pub(crate) enum NotificationReason {
    Added,
    DateChanged { old_date: Option<ReleaseDate> },
    Today,
    InOneWeek,
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn sample_notification() -> ReleaseNotification {
        ReleaseNotification {
            subscription_name: "Example".to_owned(),
            release: ReleaseRecord {
                title: Some("Episode 7".to_owned()),
                date: Some(ReleaseDate::Date(
                    chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
                )),
                image_url: Some("https://example.com/cover.jpg".to_owned()),
                url: Some("https://anilist.co/anime/123".to_owned()),
            },
            reason: NotificationReason::Today,
        }
    }
}
