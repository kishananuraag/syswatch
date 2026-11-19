//! RFC 3339 UTC timestamps without a date library.
//!
//! The only timestamps this agent prints are "now" and "boot time", both of
//! which are unix seconds. Pulling `chrono` in for two calls is a dependency
//! the agent does not need, so the civil-date conversion is done here with
//! Howard Hinnant's `civil_from_days` algorithm and covered by tests against
//! known instants.

/// `2026-11-19T18:51:13Z` — the same shape `datetime.isoformat(timespec=
/// "seconds")` produces in the Python server, minus the `+00:00` suffix.
pub fn rfc3339_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let sod = unix_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Days since 1970-01-01 → (year, month, day). Valid for the whole range of
/// dates this machine can produce. Shared with the graph tick ladder, which
/// works in local-time seconds and formats civil parts by hand.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix seconds now (system clock, not monotonic — timestamps only).
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 from a civil date (Howard Hinnant's
/// `days_from_civil`, the inverse of [`civil_from_days`]).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Parse the collector's timestamp format into unix seconds.
///
/// Accepts `2026-11-19T05:30:00Z`, an optional fractional part
/// (`2026-11-19T05:30:00.250Z`) and an explicit `+HH:MM` / `-HH:MM` offset.
/// Returns `None` for anything else — a record with an unparseable timestamp
/// is skipped by the series reader, never assigned "now".
pub fn parse_rfc3339(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', 't', ' '])?;

    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    if dp.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }

    // Split the clock from any zone suffix.
    let (clock, offset_s) = if let Some(stripped) = rest.strip_suffix(['Z', 'z']) {
        (stripped, 0.0)
    } else if let Some(idx) = rest.rfind(['+', '-']) {
        let (clock, zone) = rest.split_at(idx);
        let sign = if zone.starts_with('-') { -1.0 } else { 1.0 };
        let zone = &zone[1..];
        let (h, m) = zone.split_once(':').unwrap_or((zone, "0"));
        let oh: f64 = h.parse().ok()?;
        let om: f64 = m.parse().ok()?;
        (clock, sign * (oh * 3600.0 + om * 60.0))
    } else {
        (rest, 0.0)
    };

    let mut tp = clock.split(':');
    let hh: u32 = tp.next()?.parse().ok()?;
    let mm: u32 = tp.next()?.parse().ok()?;
    let sec_field = tp.next().unwrap_or("0");
    if tp.next().is_some() {
        return None;
    }
    let (ss, frac) = match sec_field.split_once('.') {
        Some((a, b)) => (a, b),
        None => (sec_field, ""),
    };
    let ss: u32 = ss.parse().ok()?;
    let frac_s: f64 = if frac.is_empty() {
        0.0
    } else {
        format!("0.{frac}").parse().ok()?
    };
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }

    let days = days_from_civil(y, mo, d);
    Some(days as f64 * 86_400.0 + hh as f64 * 3600.0 + mm as f64 * 60.0 + ss as f64 + frac_s
        - offset_s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        // 2026-11-19T18:51:13Z — the timestamp the Python server was
        // serving /api/current with while this was written.
        assert_eq!(rfc3339_utc(1_795_114_273), "2026-11-19T18:51:13Z");
        assert_eq!(rfc3339_utc(1_795_114_273 + 86_400), "2026-11-20T18:51:13Z");
    }

    #[test]
    fn leap_day_round_trip() {
        // 2024-02-29T12:00:00Z
        assert_eq!(rfc3339_utc(1_709_208_000), "2024-02-29T12:00:00Z");
    }

    #[test]
    fn parses_the_collector_timestamp() {
        assert_eq!(parse_rfc3339("2026-11-19T05:30:00Z"), Some(1_795_066_200.0));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_rfc3339("2024-02-29T12:00:00Z"), Some(1_709_208_000.0));
        assert_eq!(
            parse_rfc3339("2026-11-19T05:30:00.250Z"),
            Some(1_795_066_200.25)
        );
        // +02:00 is 07:30 local with a 2 h offset → 05:30Z (verified against
        // python: datetime.fromtimestamp(1795066200, UTC) = 2026-11-19T05:30:00Z).
        assert_eq!(
            parse_rfc3339("2026-11-19T07:30:00+02:00"),
            Some(1_795_066_200.0)
        );
    }

    #[test]
    fn round_trips_every_second_shape() {
        for ts in [0i64, 1, 1_000_000_000, 1_795_114_273, 4_000_000_000] {
            assert_eq!(
                parse_rfc3339(&rfc3339_utc(ts)),
                Some(ts as f64),
                "round trip failed for {ts}"
            );
        }
    }

    #[test]
    fn rejects_junk_instead_of_defaulting_to_now() {
        for junk in ["", "not-a-time", "2026-11-19", "2026-13-01T00:00:00Z", "2026-11-19T99:00:00Z"] {
            assert_eq!(parse_rfc3339(junk), None, "accepted junk {junk:?}");
        }
    }
}
