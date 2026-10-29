//! Simulated time helpers. Simulation logic works in whole epoch seconds (`i64`); k8s-openapi
//! `Time` is only used at the edges (manifests, printers).

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;

pub fn secs(time: &Time) -> i64 {
    time.0.as_second()
}

pub fn to_time(secs: i64) -> Time {
    Time(Timestamp::from_second(secs).unwrap_or(Timestamp::UNIX_EPOCH))
}

/// RFC 3339, e.g. `2026-10-27T18:30:00Z`.
pub fn format(secs: i64) -> String {
    to_time(secs).0.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_seconds() {
        let t = to_time(1_793_125_800);
        assert_eq!(secs(&t), 1_793_125_800);
        assert_eq!(format(1_793_125_800), "2026-10-27T18:30:00Z");
    }
}
