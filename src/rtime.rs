//! Time helpers: RFC 3339 timestamps like Go's time.Time JSON encoding, and
//! Go-style duration strings ("60s", "1m30s", "100ms").

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A UTC instant, stored as nanoseconds since the Unix epoch. Serializes to
/// RFC 3339 with a `Z` suffix, matching Go's encoding of a UTC time.Time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Time(pub i128);

impl Time {
    pub fn now() -> Time {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Time(d.as_nanos() as i128)
    }
    pub fn from_unix(sec: i64, nanos: u32) -> Time {
        Time(sec as i128 * 1_000_000_000 + nanos as i128)
    }
    pub fn unix_seconds(&self) -> i64 {
        (self.0.div_euclid(1_000_000_000)) as i64
    }
    pub fn sub(&self, other: Time) -> Duration {
        let d = self.0 - other.0;
        if d <= 0 {
            Duration::ZERO
        } else {
            Duration::from_nanos(d as u64)
        }
    }
}

/// Days from civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = (y_adj - era * 400) as i64;
    let mp = ((m + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil date from days since epoch.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.0.div_euclid(1_000_000_000);
        let nanos = self.0.rem_euclid(1_000_000_000) as u32;
        let days = secs.div_euclid(86400);
        let tod = secs.rem_euclid(86400);
        let (y, mo, d) = civil_from_days(days as i64);
        let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
        if nanos == 0 {
            write!(f, "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
        } else {
            let mut frac = format!("{nanos:09}");
            while frac.ends_with('0') {
                frac.pop();
            }
            write!(f, "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{frac}Z")
        }
    }
}

impl Time {
    pub fn parse(s: &str) -> Result<Time, String> {
        // 2026-10-10T19:59:25[.frac](Z|±hh:mm)
        let b = s.as_bytes();
        if b.len() < 20 {
            return Err(format!("bad time {s:?}"));
        }
        let num = |a: usize, n: usize| -> Result<i64, String> {
            s[a..a + n]
                .parse::<i64>()
                .map_err(|_| format!("bad time {s:?}"))
        };
        let (y, mo, d) = (num(0, 4)?, num(5, 2)? as u32, num(8, 2)? as u32);
        let (h, mi, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
        let mut idx = 19;
        let mut nanos: i128 = 0;
        if b.get(idx) == Some(&b'.') {
            idx += 1;
            let start = idx;
            while idx < b.len() && b[idx].is_ascii_digit() {
                idx += 1;
            }
            let mut digits = s[start..idx].to_string();
            while digits.len() < 9 {
                digits.push('0');
            }
            nanos = digits[..9].parse::<i128>().map_err(|_| "bad frac")?;
        }
        let mut offset_secs: i64 = 0;
        match b.get(idx) {
            Some(&b'Z') | None => {}
            Some(&c) if c == b'+' || c == b'-' => {
                let oh = num(idx + 1, 2)?;
                let om = num(idx + 4, 2)?;
                offset_secs = (oh * 3600 + om * 60) * if c == b'-' { -1 } else { 1 };
            }
            _ => return Err(format!("bad time {s:?}")),
        }
        let days = days_from_civil(y, mo, d);
        let secs = days * 86400 + h * 3600 + mi * 60 + sec - offset_secs;
        Ok(Time(secs as i128 * 1_000_000_000 + nanos))
    }
}

impl Serialize for Time {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Time, D::Error> {
        let s = String::deserialize(de)?;
        if s.is_empty() || s.starts_with("0001-01-01") {
            return Ok(Time(0));
        }
        Time::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Parse a Go duration string: sequences of <number><unit> with units
/// ns, us, µs, ms, s, m, h. A bare "0" is also valid.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    if s.is_empty() {
        return Err("empty duration".into());
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut total = Duration::ZERO;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        if start == i {
            return Err(format!("bad duration {s:?}"));
        }
        let value: f64 = s[start..i].parse().map_err(|_| format!("bad duration {s:?}"))?;
        let ustart = i;
        while i < bytes.len() && !bytes[i].is_ascii_digit() && bytes[i] != b'.' {
            i += 1;
        }
        let unit = &s[ustart..i];
        let mult: f64 = match unit {
            "ns" => 1.0,
            "us" | "µs" | "μs" => 1_000.0,
            "ms" => 1_000_000.0,
            "s" => 1_000_000_000.0,
            "m" => 60e9,
            "h" => 3600e9,
            _ => return Err(format!("unknown unit {unit:?} in duration {s:?}")),
        };
        total += Duration::from_nanos((value * mult) as u64);
    }
    Ok(total)
}

/// Format a duration the way Go's Duration.String does, roughly: the largest
/// units that keep it readable ("60s" -> "1m0s", "500ms" stays "500ms").
pub fn format_duration(d: Duration) -> String {
    let nanos = d.as_nanos();
    if nanos == 0 {
        return "0s".to_string();
    }
    if nanos < 1_000_000 {
        if nanos % 1000 == 0 {
            return format!("{}µs", nanos / 1000);
        }
        return format!("{}ns", nanos);
    }
    if nanos < 1_000_000_000 {
        let ms = nanos as f64 / 1e6;
        if ms.fract() == 0.0 {
            return format!("{}ms", ms as u64);
        }
        return format!("{ms}ms");
    }
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let frac = d.subsec_nanos();
    let frac_str = if frac == 0 {
        String::new()
    } else {
        let mut t = format!(".{frac:09}");
        while t.ends_with('0') {
            t.pop();
        }
        t
    };
    if h > 0 {
        format!("{h}h{m}m{s}{frac_str}s")
    } else if m > 0 {
        format!("{m}m{s}{frac_str}s")
    } else {
        format!("{s}{frac_str}s")
    }
}

pub fn human_age(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 48 * 3600 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_round_trips() {
        for (s, want) in [
            ("60s", Duration::from_secs(60)),
            ("100ms", Duration::from_millis(100)),
            ("1m30s", Duration::from_secs(90)),
            ("2h", Duration::from_secs(7200)),
            ("1h30m", Duration::from_secs(5400)),
            ("250us", Duration::from_micros(250)),
            ("0", Duration::ZERO),
        ] {
            assert_eq!(parse_duration(s).unwrap(), want, "{s}");
        }
        assert!(parse_duration("").is_err());
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("s").is_err());
    }

    #[test]
    fn duration_formats() {
        assert_eq!(format_duration(Duration::from_secs(60)), "1m0s");
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_duration(Duration::ZERO), "0s");
    }

    #[test]
    fn time_rfc3339_round_trip() {
        let t = Time::from_unix(1_789_000_000, 123_456_789);
        let s = t.to_string();
        assert!(s.starts_with("20"), "{s}");
        assert!(s.ends_with('Z'), "{s}");
        assert_eq!(Time::parse(&s).unwrap(), t);
        // Go writes whole seconds without a fraction.
        let whole = Time::from_unix(1_789_000_000, 0);
        assert!(!whole.to_string().contains('.'));
        assert_eq!(Time::parse(&whole.to_string()).unwrap(), whole);
    }

    #[test]
    fn time_parse_offsets() {
        let a = Time::parse("2026-01-02T03:04:05Z").unwrap();
        let b = Time::parse("2026-01-02T05:04:05+02:00").unwrap();
        assert_eq!(a, b);
        let c = Time::parse("2026-01-02T00:04:05-03:00").unwrap();
        assert_eq!(a, c);
    }

    #[test]
    fn ages() {
        assert_eq!(human_age(Duration::from_secs(5)), "5s");
        assert_eq!(human_age(Duration::from_secs(300)), "5m");
        assert_eq!(human_age(Duration::from_secs(7200)), "2h");
        assert_eq!(human_age(Duration::from_secs(3 * 86400)), "3d");
    }

    #[test]
    fn sub_clamps() {
        let a = Time::from_unix(100, 0);
        let b = Time::from_unix(50, 0);
        assert_eq!(a.sub(b), Duration::from_secs(50));
        assert_eq!(b.sub(a), Duration::ZERO);
    }
}
