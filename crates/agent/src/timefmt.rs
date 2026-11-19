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
/// dates this machine can produce.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
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
}
