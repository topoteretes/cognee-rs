//! Shared deserializer utilities re-used across multiple pipeline-router DTOs.

use serde::{Deserialize, Deserializer, Serialize, de};
use uuid::Uuid;

// ─── iso8601_offset (Decision 6) ──────────────────────────────────────────────

/// Serde helper module for `chrono::DateTime<Utc>` fields whose wire format
/// must match Python's `datetime.isoformat()` shape.
///
/// Python's pydantic `OutDTO.model_dump()` calls `datetime.isoformat()` which
/// emits an explicit `+00:00` offset and microsecond precision (e.g.
/// `"2026-04-29T14:32:01.123456+00:00"`). chrono's default `Serialize` impl
/// instead emits `"…Z"` with nanosecond precision, which causes byte-level
/// drift against the Python SDK on every wire-visible timestamp.
///
/// This helper is the project-wide remedy (per
/// [`docs/http-api-v2/README.md` §1.1 — Decision 6](../../../../docs/http-api-v2/README.md#11-wire-conventions-project-wide-set-by-decision-6)):
///
/// - **Serialization**: emits `%Y-%m-%dT%H:%M:%S%.6f%:z` — explicit `+00:00`
///   offset, microsecond precision, truncating any sub-microsecond digits.
/// - **Deserialization**: leniently accepts any RFC 3339 string via
///   `chrono::DateTime::parse_from_rfc3339`, so both `"…+00:00"` and `"…Z"`
///   round-trip cleanly. The parsed timestamp is converted to UTC.
///
/// # Usage
///
/// ```rust,ignore
/// use chrono::{DateTime, Utc};
///
/// #[derive(Serialize, Deserialize)]
/// struct MyDto {
///     #[serde(with = "crate::dto::util::iso8601_offset")]
///     created_at: DateTime<Utc>,
/// }
/// ```
pub mod iso8601_offset {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer, de};

    /// RFC 3339 with explicit `+00:00` offset and microsecond precision.
    ///
    /// `%.6f` truncates fractional seconds to 6 digits (microseconds), matching
    /// Python's default `datetime.isoformat()` output for non-naive UTC values.
    pub fn serialize<S>(dt: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let formatted = dt.format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string();
        s.serialize_str(&formatted)
    }

    /// Parse any RFC 3339 timestamp (with `Z` or numeric offset) and convert
    /// to UTC. Returns a serde error on malformed input.
    pub fn deserialize<'de, D>(d: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|err| {
                de::Error::custom(format!(
                    "invalid RFC 3339 timestamp {s:?}: {err}",
                    s = s,
                    err = err
                ))
            })
    }
}

// ─── DatasetIdRef ─────────────────────────────────────────────────────────────

/// A nullable dataset-id field that accepts three forms:
///
/// | Wire value | Deserialises to |
/// |---|---|
/// | `null` (JSON null) | `None` |
/// | `""` (empty string) | `None` |
/// | `"<valid UUID>"` | `Some(<uuid>)` |
///
/// Any other string — non-UUID, non-empty — is a deserialization error.
///
/// This matches Python's `Optional[UUID]` behaviour combined with the
/// empty-string normalization applied by several FastAPI endpoints.
///
/// # Usage
///
/// ```rust,ignore
/// #[derive(Deserialize)]
/// struct MyPayload {
///     #[serde(default)]
///     dataset_id: DatasetIdRef,
/// }
/// ```
///
/// The newtype wraps `Option<Uuid>` and is `#[repr(transparent)]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatasetIdRef(pub Option<Uuid>);

impl Serialize for DatasetIdRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl DatasetIdRef {
    /// Consume the newtype and return the inner `Option<Uuid>`.
    pub fn into_inner(self) -> Option<Uuid> {
        self.0
    }

    /// Borrow the inner `Option<Uuid>`.
    pub fn as_option(&self) -> Option<Uuid> {
        self.0
    }
}

impl From<DatasetIdRef> for Option<Uuid> {
    fn from(d: DatasetIdRef) -> Self {
        d.0
    }
}

// ─── OpenAPI schema ───────────────────────────────────────────────────────────

impl utoipa::ToSchema for DatasetIdRef {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DatasetIdRef")
    }
}

impl utoipa::PartialSchema for DatasetIdRef {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::Schema> {
        // Represented as a nullable UUID string in the OpenAPI spec.
        utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(
            utoipa::openapi::ObjectBuilder::new()
                .schema_type(utoipa::openapi::schema::Type::String)
                .description(Some(
                    "Optional dataset UUID. Null, empty string, or a valid UUID string.",
                ))
                .build(),
        ))
    }
}

impl<'de> Deserialize<'de> for DatasetIdRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // We accept: JSON null, empty string, valid UUID string.
        // We reject: any other non-empty string.
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            None => Ok(DatasetIdRef(None)),
            Some(s) if s.trim().is_empty() => Ok(DatasetIdRef(None)),
            Some(s) => {
                let uuid = Uuid::parse_str(&s).map_err(|_| {
                    de::Error::custom(format!(
                        "invalid dataset_id: expected a UUID string or empty, got {:?}",
                        s
                    ))
                })?;
                Ok(DatasetIdRef(Some(uuid)))
            }
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct Wrapper {
        #[serde(default)]
        id: DatasetIdRef,
    }

    fn parse(v: serde_json::Value) -> Result<DatasetIdRef, serde_json::Error> {
        #[derive(Deserialize)]
        struct W {
            id: DatasetIdRef,
        }
        let w: W = serde_json::from_value(json!({ "id": v }))?;
        Ok(w.id)
    }

    #[test]
    fn null_deserialises_to_none() {
        let result = parse(json!(null)).expect("should succeed");
        assert_eq!(result, DatasetIdRef(None));
    }

    #[test]
    fn empty_string_deserialises_to_none() {
        let result = parse(json!("")).expect("should succeed");
        assert_eq!(result, DatasetIdRef(None));
    }

    #[test]
    fn whitespace_only_string_deserialises_to_none() {
        let result = parse(json!("   ")).expect("should succeed");
        assert_eq!(result, DatasetIdRef(None));
    }

    #[test]
    fn valid_uuid_deserialises_to_some() {
        let id = Uuid::new_v4();
        let result = parse(json!(id.to_string())).expect("should succeed");
        assert_eq!(result, DatasetIdRef(Some(id)));
    }

    #[test]
    fn invalid_uuid_string_is_rejected() {
        let err = parse(json!("not-a-uuid")).expect_err("should fail");
        assert!(
            err.to_string().contains("invalid dataset_id"),
            "error message should mention the field: {err}"
        );
    }

    #[test]
    fn non_string_scalar_is_rejected() {
        let err = parse(json!(42)).expect_err("should fail for integer");
        // serde reports a type mismatch
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn default_is_none() {
        let w: Wrapper = serde_json::from_str("{}").expect("empty object");
        assert_eq!(w.id, DatasetIdRef(None));
    }

    #[test]
    fn into_inner_works() {
        let id = Uuid::new_v4();
        let d = DatasetIdRef(Some(id));
        assert_eq!(d.into_inner(), Some(id));

        let none = DatasetIdRef(None);
        assert_eq!(none.into_inner(), None);
    }

    // ─── iso8601_offset (Decision 6) ─────────────────────────────────────────

    use chrono::{DateTime, TimeZone, Utc};
    use serde::Serialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct TsWrapper {
        #[serde(with = "super::iso8601_offset")]
        ts: DateTime<Utc>,
    }

    #[test]
    fn serializes_utc_with_plus_zero_zero() {
        // 2026-04-29T14:32:01Z -> "2026-04-29T14:32:01.000000+00:00"
        let ts = Utc
            .with_ymd_and_hms(2026, 4, 29, 14, 32, 1)
            .single()
            .expect("valid UTC datetime");
        let w = TsWrapper { ts };
        let s = serde_json::to_string(&w).expect("serialize");
        assert!(
            s.contains("\"2026-04-29T14:32:01.000000+00:00\""),
            "expected +00:00 offset in: {s}"
        );
        assert!(
            !s.contains("Z\""),
            "should not emit chrono's default Z suffix: {s}"
        );
    }

    #[test]
    fn deserializes_z_suffix() {
        let json = r#"{"ts":"2026-04-29T14:32:01Z"}"#;
        let w: TsWrapper = serde_json::from_str(json).expect("Z suffix should parse");
        let expected = Utc
            .with_ymd_and_hms(2026, 4, 29, 14, 32, 1)
            .single()
            .expect("valid UTC datetime");
        assert_eq!(w.ts, expected);
    }

    #[test]
    fn deserializes_plus_zero_zero() {
        let json = r#"{"ts":"2026-04-29T14:32:01+00:00"}"#;
        let w: TsWrapper = serde_json::from_str(json).expect("+00:00 offset should parse");
        let expected = Utc
            .with_ymd_and_hms(2026, 4, 29, 14, 32, 1)
            .single()
            .expect("valid UTC datetime");
        assert_eq!(w.ts, expected);
    }

    #[test]
    fn round_trip_microsecond_precision() {
        let json = r#"{"ts":"2026-04-29T14:32:01.123456+00:00"}"#;
        let w: TsWrapper = serde_json::from_str(json).expect("microsecond input");
        let s = serde_json::to_string(&w).expect("serialize");
        assert!(
            s.contains("\"2026-04-29T14:32:01.123456+00:00\""),
            "round-trip should preserve microseconds: {s}"
        );
    }

    #[test]
    fn truncates_nanoseconds_to_microseconds_on_serialize() {
        // Build a datetime carrying 123_456_789 ns; the helper must drop the
        // last three digits ("789") so the wire matches Python microseconds.
        let ts = Utc
            .with_ymd_and_hms(2026, 4, 29, 14, 32, 1)
            .single()
            .expect("valid UTC datetime")
            + chrono::Duration::nanoseconds(123_456_789);
        let w = TsWrapper { ts };
        let s = serde_json::to_string(&w).expect("serialize");
        assert!(
            s.contains("\"2026-04-29T14:32:01.123456+00:00\""),
            "expected microsecond truncation, got: {s}"
        );
        assert!(
            !s.contains("123456789"),
            "nanoseconds should be truncated, got: {s}"
        );
    }
}
