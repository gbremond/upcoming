use chrono::NaiveDate;

#[derive(Debug, PartialEq)]
pub(crate) enum Reminder {
    Today,
    InOneWeek,
}

pub(crate) fn reminder_for(release_date: NaiveDate, today: NaiveDate) -> Option<Reminder> {
    let days = (release_date - today).num_days();

    match days {
        0 => Some(Reminder::Today),
        7 => Some(Reminder::InOneWeek),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminds_only_on_release_day_and_one_week_before() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 7).unwrap();

        for (day, expected) in [
            (6, None), // yesterday
            (7, Some(Reminder::Today)),
            (13, None), // six days away
            (14, Some(Reminder::InOneWeek)),
            (15, None), // eight days away
        ] {
            let release = NaiveDate::from_ymd_opt(2026, 11, day).unwrap();
            assert_eq!(reminder_for(release, today), expected);
        }
    }
}
