//! Calendar rules. All timestamps are unix seconds (UTC). Calendar-month
//! arithmetic is done in Asia/Jakarta (fixed UTC+7, no DST), as required by
//! the proposal (section 10).

pub const DAY: i64 = 86_400;
pub const WEEK: i64 = 7 * DAY;
/// Asia/Jakarta (WIB) has no daylight saving time, so a fixed offset is exact.
pub const JAKARTA_OFFSET: i64 = 7 * 3600;
pub const ELECTION_WEEKS: i64 = 4;
pub const ELECTION_LEN: i64 = ELECTION_WEEKS * WEEK;
pub const GENERATION_MONTHS: i64 = 6;

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Unix timestamp for a Jakarta wall-clock time.
pub fn wib_ts(y: i64, m: i64, d: i64, hh: i64, mm: i64) -> i64 {
    days_from_civil(y, m, d) * DAY + hh * 3600 + mm * 60 - JAKARTA_OFFSET
}

/// Add calendar months in Jakarta local time, keeping the wall-clock time and
/// clamping the day to the end of shorter months (31 Aug + 6 months = 28/29 Feb).
pub fn add_months(ts: i64, months: i64) -> i64 {
    let local = ts + JAKARTA_OFFSET;
    let days = local.div_euclid(DAY);
    let secs = local.rem_euclid(DAY);
    let (y, m, d) = civil_from_days(days);
    let total = y * 12 + (m - 1) + months;
    let ny = total.div_euclid(12);
    let nm = total.rem_euclid(12) + 1;
    let nd = d.min(days_in_month(ny, nm));
    days_from_civil(ny, nm, nd) * DAY + secs - JAKARTA_OFFSET
}

/// End of a generation that starts at `start`.
pub fn generation_end(start: i64) -> i64 {
    add_months(start, GENERATION_MONTHS)
}

/// The election for generation N+1 occupies the last four weeks of generation N.
pub fn election_start_for(generation_end: i64) -> i64 {
    generation_end - ELECTION_LEN
}

/// How many weekly selections are due at `now` for an election starting at
/// `starts_at`: 0..=4. Selection `k` becomes due at `starts_at + k weeks`.
pub fn stage_due(starts_at: i64, now: i64) -> u8 {
    if now < starts_at {
        0
    } else {
        ((now - starts_at) / WEEK).min(ELECTION_WEEKS) as u8
    }
}

/// Votes are frozen from the start of week 4 until the election is finalized.
/// There is deliberately no upper bound: the caller only asks while the
/// election is still `running`, so the short gap between `ends_at` and the
/// scheduler finalizing it can never be used to cast late votes.
pub fn votes_locked(starts_at: i64, now: i64) -> bool {
    now >= starts_at + (ELECTION_WEEKS - 1) * WEEK
}

/// `2026-10-10 17:44 WIB`
pub fn format_wib(ts: i64) -> String {
    let local = ts + JAKARTA_OFFSET;
    let (y, m, d) = civil_from_days(local.div_euclid(DAY));
    let s = local.rem_euclid(DAY);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} WIB", s / 3600, (s % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_epoch_values() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 1, 1) * DAY, 1_767_225_600);
        // 2026-01-01 00:00 WIB is 7 hours before 00:00 UTC.
        assert_eq!(wib_ts(2026, 1, 1, 0, 0), 1_767_225_600 - 7 * 3600);
    }

    #[test]
    fn civil_roundtrip() {
        for z in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn add_months_basic_and_clamp() {
        let a = wib_ts(2026, 10, 10, 17, 44);
        assert_eq!(add_months(a, 6), wib_ts(2027, 4, 10, 17, 44));
        // 31 Aug 2026 + 6 months clamps to 28 Feb 2027.
        assert_eq!(add_months(wib_ts(2026, 8, 31, 9, 0), 6), wib_ts(2027, 2, 28, 9, 0));
        // Leap year clamp.
        assert_eq!(add_months(wib_ts(2027, 8, 31, 9, 0), 6), wib_ts(2028, 2, 29, 9, 0));
        // Year wrap.
        assert_eq!(add_months(wib_ts(2026, 12, 15, 0, 0), 6), wib_ts(2027, 6, 15, 0, 0));
    }

    #[test]
    fn add_months_uses_jakarta_date_not_utc_date() {
        // 2026-08-31 02:00 WIB is still 2026-08-30 in UTC; Jakarta date must win.
        let t = wib_ts(2026, 8, 31, 2, 0);
        assert_eq!(add_months(t, 6), wib_ts(2027, 2, 28, 2, 0));
    }

    #[test]
    fn election_window_is_last_four_weeks() {
        let start = wib_ts(2026, 11, 7, 0, 0);
        let end = generation_end(start);
        assert_eq!(election_start_for(end), end - 28 * DAY);
    }

    #[test]
    fn stage_due_progression() {
        let s = 1_000_000;
        assert_eq!(stage_due(s, s - 1), 0);
        assert_eq!(stage_due(s, s), 0);
        assert_eq!(stage_due(s, s + WEEK - 1), 0);
        assert_eq!(stage_due(s, s + WEEK), 1);
        assert_eq!(stage_due(s, s + 2 * WEEK), 2);
        assert_eq!(stage_due(s, s + 3 * WEEK), 3);
        assert_eq!(stage_due(s, s + 4 * WEEK), 4);
        assert_eq!(stage_due(s, s + 400 * WEEK), 4);
    }

    #[test]
    fn votes_locked_from_week_four() {
        let s = 1_000_000;
        assert!(!votes_locked(s, s + 3 * WEEK - 1));
        assert!(votes_locked(s, s + 3 * WEEK));
        assert!(votes_locked(s, s + ELECTION_LEN - 1));
        // Still locked after ends_at until the scheduler finalizes.
        assert!(votes_locked(s, s + ELECTION_LEN + 30));
    }

    #[test]
    fn formatting() {
        assert_eq!(format_wib(wib_ts(2026, 10, 10, 17, 44)), "2026-10-10 17:44 WIB");
    }
}
