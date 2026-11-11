//! A caller's `response_schema` for structured completions: compiled once,
//! then used to conform each answer to it.
//!
//! Only a structural subset is accepted: objects, primitives, arrays, enums,
//! unions (`anyOf`, a `type` list) and `$defs` references. Anything else is a
//! [`ResponseSchemaError`]. Conforming fills absent optional fields with
//! `null` and drops undeclared keys. It is strict about types: unlike
//! Python's Pydantic (lax mode), a string is never coerced to a number or a
//! boolean.

use serde_json::{Map, Value};

/// Why a `response_schema` was refused: `response_schema: <reason>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("response_schema: {0}")]
pub struct ResponseSchemaError(String);

/// A compiled `response_schema`: the root object's shape.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseSchema {
    root: ObjectShape,
}

/// One compiled schema node.
#[derive(Debug, Clone, PartialEq)]
enum Shape {
    Null,
    String,
    Integer,
    Number,
    Boolean,
    Array(Box<Shape>),
    Enum(Vec<serde_json::Value>),
    Union(Vec<Shape>),
    Object(ObjectShape),
}

/// An object node: its declared properties in schema order. A field not in
/// `required` may be absent or `null`.
#[derive(Debug, Clone, PartialEq)]
struct ObjectShape {
    fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq)]
struct Field {
    name: String,
    shape: Shape,
    required: bool,
}

impl ResponseSchema {
    /// Compile `schema`, refusing anything outside the supported subset.
    pub fn compile(schema: &Value) -> Result<Self, ResponseSchemaError> {
        let Value::Object(root) = schema else {
            return Err(fail("must be a JSON object"));
        };
        if root.get("type").and_then(Value::as_str) != Some("object") {
            return Err(fail("root schema must have type 'object'"));
        }
        // The first truthy of `$defs` and `definitions`; neither means none.
        let empty = Map::new();
        let defs = [root.get("$defs"), root.get("definitions")]
            .into_iter()
            .flatten()
            .find(|value| truthy(value));
        let defs = match defs {
            None => &empty,
            Some(Value::Object(defs)) => defs,
            Some(_) => return Err(fail("'$defs' must be an object")),
        };
        let root = Compiler::new(defs).object(root, 0, &[])?;
        Ok(Self { root })
    }

    /// Check `answer` against the schema and return it normalised: every
    /// declared field present (absent optional ones as `null`), undeclared
    /// keys dropped, an integer where a number is declared as a float. The
    /// error names the path that failed, e.g. `people[0].role: expected a string`.
    pub fn conform(&self, answer: &Value) -> Result<Value, String> {
        conform_object(&self.root, answer, "")
    }
}

fn conform_object(shape: &ObjectShape, value: &Value, path: &str) -> Result<Value, String> {
    let Value::Object(given) = value else {
        return Err(mismatch(path, "an object"));
    };
    let mut out = Map::with_capacity(shape.fields.len());
    for field in &shape.fields {
        let field_path = child_path(path, &field.name);
        let conformed = match given.get(&field.name) {
            None if field.required => return Err(format!("{field_path}: missing required field")),
            None => Value::Null,
            Some(Value::Null) if !field.required => Value::Null,
            Some(value) => conform_shape(&field.shape, value, &field_path)?,
        };
        out.insert(field.name.clone(), conformed);
    }
    Ok(Value::Object(out))
}

fn conform_shape(shape: &Shape, value: &Value, path: &str) -> Result<Value, String> {
    match (shape, value) {
        (Shape::Null, Value::Null)
        | (Shape::String, Value::String(_))
        | (Shape::Boolean, Value::Bool(_)) => Ok(value.clone()),
        (Shape::Integer, Value::Number(n)) if n.is_i64() || n.is_u64() => Ok(value.clone()),
        (Shape::Number, Value::Number(n)) => Ok(n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| value.clone(), Value::Number)),
        (Shape::Array(items), Value::Array(given)) => given
            .iter()
            .enumerate()
            .map(|(i, item)| conform_shape(items, item, &format!("{path}[{i}]")))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        (Shape::Enum(allowed), _) if allowed.contains(value) => Ok(value.clone()),
        (Shape::Union(members), _) => members
            .iter()
            .find_map(|member| conform_shape(member, value, path).ok())
            .ok_or_else(|| mismatch(path, "a value matching one of the allowed types")),
        (Shape::Object(object), _) => conform_object(object, value, path),
        (shape, _) => Err(mismatch(path, expected(shape))),
    }
}

fn child_path(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}.{name}")
    }
}

fn mismatch(path: &str, expected: &str) -> String {
    if path.is_empty() {
        format!("expected {expected}")
    } else {
        format!("{path}: expected {expected}")
    }
}

fn expected(shape: &Shape) -> &'static str {
    match shape {
        Shape::Null => "null",
        Shape::String => "a string",
        Shape::Integer => "an integer",
        Shape::Number => "a number",
        Shape::Boolean => "a boolean",
        Shape::Array(_) => "an array",
        Shape::Enum(_) => "one of the enum values",
        Shape::Union(_) => "a value matching one of the allowed types",
        Shape::Object(_) => "an object",
    }
}

/// The deepest nesting and the most properties a schema may have.
const MAX_DEPTH: usize = 10;
const MAX_TOTAL_PROPERTIES: usize = 200;

/// Keywords refused wherever they appear, checked in this order.
const UNSUPPORTED_KEYWORDS: [&str; 4] = ["allOf", "oneOf", "not", "patternProperties"];

/// One compilation: the schema's `$defs` and the properties counted so far
/// across the whole schema.
struct Compiler<'a> {
    defs: &'a Map<String, Value>,
    properties: usize,
}

impl<'a> Compiler<'a> {
    fn new(defs: &'a Map<String, Value>) -> Self {
        Self {
            defs,
            properties: 0,
        }
    }

    /// Compile an object node's properties; each is compiled at `depth`.
    fn object(
        &mut self,
        schema: &Map<String, Value>,
        depth: usize,
        in_flight: &[&str],
    ) -> Result<ObjectShape, ResponseSchemaError> {
        let properties = match schema.get("properties") {
            Some(Value::Object(properties)) if !properties.is_empty() => properties,
            _ => return Err(fail("object schemas must declare non-empty 'properties'")),
        };
        self.spend(properties.len())?;
        let required: Vec<&str> = match schema.get("required") {
            Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let mut fields = Vec::with_capacity(properties.len());
        for (name, property) in properties {
            if !is_identifier(name) {
                return Err(fail(format!(
                    "property name '{name}' is not a valid identifier"
                )));
            }
            fields.push(Field {
                name: name.clone(),
                shape: self.node(property, depth, in_flight)?,
                required: required.contains(&name.as_str()),
            });
        }
        Ok(ObjectShape { fields })
    }

    /// Compile one schema node.
    fn node(
        &mut self,
        schema: &Value,
        depth: usize,
        in_flight: &[&str],
    ) -> Result<Shape, ResponseSchemaError> {
        if depth > MAX_DEPTH {
            return Err(fail(format!("nesting deeper than {MAX_DEPTH} levels")));
        }
        let Value::Object(node) = schema else {
            return Err(fail("every schema node must be an object"));
        };
        if let Some(keyword) = UNSUPPORTED_KEYWORDS.iter().find(|k| node.contains_key(**k)) {
            return Err(fail(format!("unsupported JSON Schema keyword: {keyword}")));
        }
        if let Some(reference) = node.get("$ref") {
            return self.reference(reference, depth, in_flight);
        }
        if let Some(values) = node.get("enum") {
            return enum_shape(values);
        }
        if let Some(members) = node.get("anyOf") {
            return self.any_of(members, depth, in_flight);
        }
        match node.get("type") {
            Some(Value::Array(types)) => self.type_list(node, types, depth, in_flight),
            schema_type => self.typed(node, schema_type, depth, in_flight),
        }
    }

    /// Compile the target of a `$ref`, which `in_flight` must not contain.
    fn reference(
        &mut self,
        reference: &Value,
        depth: usize,
        in_flight: &[&str],
    ) -> Result<Shape, ResponseSchemaError> {
        let text = reference.as_str().unwrap_or_default();
        let Some(name) = ["#/$defs/", "#/definitions/"]
            .iter()
            .find_map(|prefix| text.strip_prefix(prefix))
        else {
            return Err(fail(format!(
                "unsupported reference '{text}' (only #/$defs/... and #/definitions/...)"
            )));
        };
        let defs = self.defs;
        let Some(target) = defs.get(name) else {
            return Err(fail(format!("unresolvable reference '{text}'")));
        };
        if in_flight.contains(&name) {
            return Err(fail(format!("recursive reference '{text}'")));
        }
        let in_flight: Vec<&str> = in_flight.iter().copied().chain([name]).collect();
        self.node(target, depth + 1, &in_flight)
    }

    /// Compile `anyOf` members into a union.
    fn any_of(
        &mut self,
        members: &Value,
        depth: usize,
        in_flight: &[&str],
    ) -> Result<Shape, ResponseSchemaError> {
        let Value::Array(members) = members else {
            return Err(fail("every schema node must be an object"));
        };
        let members = members
            .iter()
            .map(|member| self.node(member, depth + 1, in_flight))
            .collect::<Result<_, _>>()?;
        Ok(Shape::Union(members))
    }

    /// Compile a node whose `type` is a list into a union, one member per
    /// type, each a copy of the node with that single `type`.
    fn type_list(
        &mut self,
        node: &Map<String, Value>,
        types: &[Value],
        depth: usize,
        in_flight: &[&str],
    ) -> Result<Shape, ResponseSchemaError> {
        let members = types
            .iter()
            .map(|single| {
                let mut member = node.clone();
                member.insert("type".to_string(), single.clone());
                self.node(&Value::Object(member), depth + 1, in_flight)
            })
            .collect::<Result<_, _>>()?;
        Ok(Shape::Union(members))
    }

    /// Compile a node with a single (or no) `type`.
    fn typed(
        &mut self,
        node: &Map<String, Value>,
        schema_type: Option<&Value>,
        depth: usize,
        in_flight: &[&str],
    ) -> Result<Shape, ResponseSchemaError> {
        match schema_type.and_then(Value::as_str) {
            Some("null") => Ok(Shape::Null),
            Some("string") => Ok(Shape::String),
            Some("integer") => Ok(Shape::Integer),
            Some("number") => Ok(Shape::Number),
            Some("boolean") => Ok(Shape::Boolean),
            Some("array") => match node.get("items") {
                Some(items) => Ok(Shape::Array(Box::new(self.node(
                    items,
                    depth + 1,
                    in_flight,
                )?))),
                None => Err(fail("array schemas must declare 'items'")),
            },
            Some("object") => Ok(Shape::Object(self.object(node, depth + 1, in_flight)?)),
            _ if node.contains_key("properties") => {
                Ok(Shape::Object(self.object(node, depth + 1, in_flight)?))
            }
            _ => Err(fail(format!(
                "unsupported or missing 'type' in schema node: {}",
                schema_type.unwrap_or(&Value::Null)
            ))),
        }
    }

    /// Count `count` more properties against the whole schema's limit.
    fn spend(&mut self, count: usize) -> Result<(), ResponseSchemaError> {
        self.properties += count;
        if self.properties > MAX_TOTAL_PROPERTIES {
            return Err(fail(format!(
                "more than {MAX_TOTAL_PROPERTIES} properties in total"
            )));
        }
        Ok(())
    }
}

/// Compile an `enum`: a non-empty list of strings, integers or booleans.
fn enum_shape(values: &Value) -> Result<Shape, ResponseSchemaError> {
    let allowed = |value: &Value| match value {
        Value::String(_) | Value::Bool(_) => true,
        Value::Number(n) => n.is_i64() || n.is_u64(),
        _ => false,
    };
    match values {
        Value::Array(values) if !values.is_empty() && values.iter().all(allowed) => {
            Ok(Shape::Enum(values.clone()))
        }
        _ => Err(fail(
            "enum values must be non-empty strings, integers, or booleans",
        )),
    }
}

/// Whether `name` is a valid identifier: a letter or `_`, then letters,
/// digits or `_`.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn fail(reason: impl Into<String>) -> ResponseSchemaError {
    ResponseSchemaError(reason.into())
}

/// Whether a JSON value is truthy: `null`, `false`, `0`, `""`, `[]` and `{}`
/// are not.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What Pydantic's `model_json_schema()` emits for a recipe model with a
    /// nested model, an enum, an optional field and a defaulted literal.
    fn recipe() -> ResponseSchema {
        ResponseSchema::compile(&json!({
            "type": "object",
            "title": "Recipe",
            "$defs": {
                "Ingredient": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "grams": {"type": "number"}
                    },
                    "required": ["name", "grams"]
                },
                "Difficulty": {"type": "string", "enum": ["easy", "hard"]}
            },
            "properties": {
                "title": {"type": "string"},
                "servings": {"type": "integer"},
                "vegetarian": {"type": "boolean"},
                "difficulty": {"$ref": "#/$defs/Difficulty"},
                "ingredients": {"type": "array", "items": {"$ref": "#/$defs/Ingredient"}},
                "notes": {"anyOf": [{"type": "string"}, {"type": "null"}], "default": null},
                "kind": {"enum": ["starter", "main", "dessert"], "default": "main"}
            },
            "required": ["title", "servings", "vegetarian", "difficulty", "ingredients"]
        }))
        .expect("the recipe schema compiles")
    }

    /// The answer comes back as the validated model would dump it.
    #[test]
    fn a_valid_answer_is_normalised() {
        let answer = json!({
            "title": "Stew",
            "servings": 4,
            "vegetarian": false,
            "difficulty": "easy",
            "ingredients": [{"name": "carrot", "grams": 200}],
            "chef": "Ann"
        });
        let conformed = recipe().conform(&answer).expect("conforms");
        assert_eq!(
            conformed,
            json!({
                "title": "Stew",
                "servings": 4,
                "vegetarian": false,
                "difficulty": "easy",
                "ingredients": [{"name": "carrot", "grams": 200.0}],
                "notes": null,
                "kind": null
            })
        );
    }

    #[test]
    fn a_non_conforming_answer_names_what_failed() {
        let base = json!({
            "title": "Stew", "servings": 4, "vegetarian": false,
            "difficulty": "easy", "ingredients": []
        });
        let with = |key: &str, value: Value| {
            let mut answer = base.clone();
            answer[key] = value;
            answer
        };
        let cases = [
            (json!({"title": "Stew"}), "servings: missing required field"),
            (
                with("ingredients", json!([{"name": "carrot", "grams": "many"}])),
                "ingredients[0].grams: expected a number",
            ),
            (
                with("difficulty", json!("impossible")),
                "difficulty: expected one of the enum values",
            ),
            (
                with("servings", json!("4")),
                "servings: expected an integer",
            ),
        ];
        for (answer, error) in cases {
            assert_eq!(
                recipe().conform(&answer),
                Err(error.to_string()),
                "{answer}"
            );
        }
    }

    #[test]
    fn definitions_and_type_lists_compile() {
        let schema = ResponseSchema::compile(&json!({
            "type": "object",
            "definitions": {"Tag": {"type": "string"}},
            "properties": {
                "tag": {"$ref": "#/definitions/Tag"},
                "maybe": {"type": ["integer", "null"]}
            },
            "required": ["maybe"]
        }))
        .unwrap();
        assert_eq!(
            schema.conform(&json!({"maybe": null})),
            Ok(json!({"tag": null, "maybe": null}))
        );
        assert_eq!(
            schema.conform(&json!({"tag": "x", "maybe": "no"})),
            Err("maybe: expected a value matching one of the allowed types".to_string())
        );
    }

    #[test]
    fn a_schema_outside_the_subset_is_refused() {
        let root = |properties: Value| json!({"type": "object", "properties": properties});
        let mut nested = json!({"type": "string"});
        for _ in 0..11 {
            nested = json!({"type": "array", "items": nested});
        }
        let too_many: Map<String, Value> = (0..201)
            .map(|i| (format!("p{i}"), json!({"type": "string"})))
            .collect();
        let node = json!({"type": "object", "properties": {"child": {"$ref": "#/$defs/Node"}}});
        let cases = [
            (json!(["x"]), "must be a JSON object"),
            (
                json!({"type": "array"}),
                "root schema must have type 'object'",
            ),
            (
                json!({"type": "object", "$defs": 3}),
                "'$defs' must be an object",
            ),
            (
                json!({"type": "object", "properties": {}}),
                "object schemas must declare non-empty 'properties'",
            ),
            (
                root(json!({"a": {"allOf": []}})),
                "unsupported JSON Schema keyword: allOf",
            ),
            (
                json!({"type": "object", "$defs": {"Node": node}, "properties": {"n": {"$ref": "#/$defs/Node"}}}),
                "recursive reference '#/$defs/Node'",
            ),
            (
                root(json!({"a": {"$ref": "#/$defs/Missing"}})),
                "unresolvable reference '#/$defs/Missing'",
            ),
            (
                root(json!({"a": {"$ref": "http://x"}})),
                "unsupported reference 'http://x' (only #/$defs/... and #/definitions/...)",
            ),
            (
                root(json!({"a": {"type": "array"}})),
                "array schemas must declare 'items'",
            ),
            (
                root(Value::Object(too_many)),
                "more than 200 properties in total",
            ),
            (
                root(json!({"bad-name": {"type": "string"}})),
                "property name 'bad-name' is not a valid identifier",
            ),
            (
                root(json!({"a": {"enum": [1.5]}})),
                "enum values must be non-empty strings, integers, or booleans",
            ),
            (
                root(json!({"a": {}})),
                "unsupported or missing 'type' in schema node: null",
            ),
            (root(json!({"a": nested})), "nesting deeper than 10 levels"),
        ];
        for (schema, reason) in cases {
            let error = ResponseSchema::compile(&schema).expect_err("refused");
            assert_eq!(
                error.to_string(),
                format!("response_schema: {reason}"),
                "{schema}"
            );
        }
    }
}
