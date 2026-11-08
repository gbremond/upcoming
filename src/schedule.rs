use chrono::{DateTime, NaiveTime, TimeZone};

pub(crate) fn until_next<Tz: TimeZone>(at: NaiveTime, now: &DateTime<Tz>) -> DateTime<Tz> {
    let timezone = now.timezone();
    let mut day = now.date_naive();
    loop {
        let next = timezone.from_local_datetime(&day.and_time(at)).earliest();
        if let Some(next) = next
            && next > *now
        {
            return next;
        }
        day = day.succ_opt().expect("date out of range");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono_tz::Europe::Paris;
    use pretty_assertions::assert_eq;

    fn paris(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<chrono_tz::Tz> {
        Paris.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }
    fn at_16() -> NaiveTime {
        NaiveTime::from_hms_opt(16, 0, 0).unwrap()
    }

    #[test]
    fn later_today() {
        assert_eq!(
            until_next(at_16(), &paris(2026, 11, 8, 10, 0)),
            paris(2026, 11, 8, 16, 0)
        );
    }
    #[test]
    fn already_passed_waits_for_tomorrow() {
        assert_eq!(
            until_next(at_16(), &paris(2026, 11, 8, 17, 0)),
            paris(2026, 11, 9, 16, 0)
        );
    }
    #[test]
    fn exactly_now_waits_a_full_day() {
        // Otherwise the run would repeat instantly in a loop.
        assert_eq!(
            until_next(at_16(), &paris(2026, 11, 8, 16, 0)),
            paris(2026, 11, 9, 16, 0)
        );
    }
    #[test]
    fn dst_end_day_is_25_hours() {
        // Clocks go back on 2026-10-25 in Paris.
        assert_eq!(
            until_next(at_16(), &paris(2026, 10, 24, 16, 0)),
            paris(2026, 10, 25, 16, 0)
        );
    }
    #[test]
    fn dst_start_day_is_23_hours() {
        // Clocks go forward on 2026-03-29.
        assert_eq!(
            until_next(at_16(), &paris(2026, 3, 28, 16, 0)),
            paris(2026, 3, 29, 16, 0)
        );
    }
    #[test]
    fn skipped_local_time_moves_to_next_day() {
        // 02:30 doesn't exist on 2026-03-29 (02:00 jumps to 03:00).
        let at_2_30 = NaiveTime::from_hms_opt(2, 30, 0).unwrap();
        assert_eq!(
            until_next(at_2_30, &paris(2026, 3, 29, 0, 0)),
            paris(2026, 3, 30, 2, 30)
        );
    }
}
