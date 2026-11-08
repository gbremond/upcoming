use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(crate) enum ReleaseDate {
    Date(NaiveDate),
    Instant(DateTime<Utc>),
    Partial {
        year: Option<i32>,
        month: Option<u32>,
        day: Option<u32>,
    },
}

impl ReleaseDate {
    pub(crate) fn reminder_date(&self) -> Option<NaiveDate> {
        match self {
            Self::Date(date) => Some(*date),
            Self::Instant(instant) => Some(instant.with_timezone(&chrono::Local).date_naive()),
            Self::Partial { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn preserves_date_precision_through_json() {
        let dates = [
            ReleaseDate::Date(NaiveDate::from_ymd_opt(2026, 10, 8).unwrap()),
            ReleaseDate::Instant("2027-10-08T14:00:00Z".parse().unwrap()),
            ReleaseDate::Partial {
                year: Some(2027),
                month: Some(10),
                day: None,
            },
        ];

        for date in dates {
            let json = serde_json::to_string(&date).unwrap();
            let restored: ReleaseDate = serde_json::from_str(&json).unwrap();

            assert_eq!(restored, date);
        }
    }

    #[test]
    fn partial_dates_do_not_produce_reminder_dates() {
        let partial = ReleaseDate::Partial {
            year: Some(2027),
            month: Some(10),
            day: None,
        };

        assert_eq!(partial.reminder_date(), None);

        let date = NaiveDate::from_ymd_opt(2027, 10, 8).unwrap();

        assert_eq!(ReleaseDate::Date(date).reminder_date(), Some(date));
    }
}
