//! Shared deserializer utilities re-used across multiple pipeline-router DTOs.

use serde::{Deserialize, Deserializer, Serialize, de};
use uuid::Uuid;

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
}
