//! Docker-style byte sizes: `512m`, `1g`, `64mb`, or a raw byte count.

use serde::de::{self, Deserializer, Visitor};
use serde::Deserialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteQty(pub i64);

impl ByteQty {
    pub fn get(self) -> i64 {
        self.0
    }
}

pub fn parse_qty(raw: &str) -> Result<i64, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("empty size".into());
    }
    let lower = text.to_ascii_lowercase();
    const UNITS: &[(&str, u64)] = &[
        ("gib", 1024 * 1024 * 1024),
        ("gb", 1024 * 1024 * 1024),
        ("g", 1024 * 1024 * 1024),
        ("mib", 1024 * 1024),
        ("mb", 1024 * 1024),
        ("m", 1024 * 1024),
        ("kib", 1024),
        ("kb", 1024),
        ("k", 1024),
        ("b", 1),
    ];
    let (number, mult) = UNITS
        .iter()
        .find_map(|(suffix, mult)| lower.strip_suffix(suffix).map(|n| (n.trim(), *mult)))
        .unwrap_or((lower.as_str(), 1));
    if number.is_empty() {
        return Err(format!("invalid size: {raw}"));
    }
    let value: f64 = number.parse().map_err(|_| format!("invalid size: {raw}"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err("size must be positive".into());
    }
    let bytes = value * mult as f64;
    if bytes > i64::MAX as f64 {
        return Err("size is too large".into());
    }
    let bytes = bytes.round() as i64;
    if bytes <= 0 {
        return Err("size must be positive".into());
    }
    Ok(bytes)
}

impl<'de> Deserialize<'de> for ByteQty {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = ByteQty;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an integer byte count or a string like 512m")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<ByteQty, E> {
                if v == 0 || v > i64::MAX as u64 {
                    return Err(E::custom("size must be positive"));
                }
                Ok(ByteQty(v as i64))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<ByteQty, E> {
                if v <= 0 {
                    return Err(E::custom("size must be positive"));
                }
                Ok(ByteQty(v))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<ByteQty, E> {
                parse_qty(v).map(ByteQty).map_err(E::custom)
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_units() {
        assert_eq!(parse_qty("512m").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_qty("64MB").unwrap(), 64 * 1024 * 1024);
        assert_eq!(parse_qty("1g").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(
            parse_qty("1.5g").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as i64
        );
        assert_eq!(parse_qty("1024").unwrap(), 1024);
        assert_eq!(parse_qty("2k").unwrap(), 2048);
        assert!(parse_qty("0").is_err());
        assert!(parse_qty("-1m").is_err());
        assert!(parse_qty("").is_err());
        assert!(parse_qty("m").is_err());
    }
}
