//! UUID ↔ 32-char hex string conversions.
//!
//! Python's SQLAlchemy stores UUIDs as 32-character hex strings (no hyphens)
//! in SQLite.  These helpers ensure the Rust database layer uses the same
//! format, so the two SDKs can share a database file.

use uuid::Uuid;

/// Convert a `Uuid` to the 32-char hex format used by Python's SQLAlchemy.
///
/// Example: `550e8400-e29b-41d4-a716-446655440000` → `"550e8400e29b41d4a716446655440000"`
#[inline]
pub fn to_hex(u: Uuid) -> String {
    u.simple().to_string()
}

/// Convert an `Option<Uuid>` to an optional hex string.
#[inline]
pub fn to_hex_opt(u: Option<Uuid>) -> Option<String> {
    u.map(to_hex)
}

/// Parse a 32-char hex string (or hyphenated UUID) back to `Uuid`.
///
/// Accepts both `"550e8400e29b41d4a716446655440000"` and
/// `"550e8400-e29b-41d4-a716-446655440000"` for backwards compatibility.
#[inline]
pub fn from_hex(s: &str) -> Result<Uuid, uuid::Error> {
    Uuid::parse_str(s)
}

/// Parse an optional hex string back to `Option<Uuid>`.
#[inline]
pub fn from_hex_opt(s: Option<&str>) -> Result<Option<Uuid>, uuid::Error> {
    match s {
        Some(s) => from_hex(s).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let u = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let hex = to_hex(u);
        assert_eq!(hex, "550e8400e29b41d4a716446655440000");
        assert_eq!(hex.len(), 32);
        assert_eq!(from_hex(&hex).unwrap(), u);
    }

    #[test]
    fn from_hex_accepts_hyphenated() {
        let u = from_hex("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(to_hex(u), "550e8400e29b41d4a716446655440000");
    }

    #[test]
    fn option_round_trip() {
        let u = Uuid::new_v4();
        assert_eq!(
            from_hex_opt(to_hex_opt(Some(u)).as_deref()).unwrap(),
            Some(u)
        );
        assert_eq!(from_hex_opt(to_hex_opt(None).as_deref()).unwrap(), None);
    }
}
