//! Request validation with safe RFC 6901 field diagnostics.
use crate::{Error, Input, MessageCode, messages::params};
use serde_json::Value;

fn pointer(name: &str) -> String {
    format!("/{}", name.replace('~', "~0").replace('/', "~1"))
}

/// Parse a request, preserving duplicate-field checks and syntax causes.
pub fn parse_input(raw: &str) -> Result<Input, Error> {
    let value: Value = serde_json::from_str(raw)?;
    let object = value.as_object().ok_or_else(|| type_error("", "object"))?;
    let op = object.get("op").ok_or_else(|| missing("/op"))?;
    if !op.is_string() {
        return Err(type_error("/op", "string"));
    }
    // The parser schema retains nullable legacy fields; the UI schema omits null.
    let schema = schemars::schema_for!(Input).to_value();
    let variants = schema["oneOf"]
        .as_array()
        .ok_or_else(|| type_error("", "object"))?;
    let variant = variants
        .iter()
        .find(|v| v["properties"]["op"]["const"] == *op)
        .ok_or_else(|| {
            Error::invalid(
                MessageCode::InputEnum,
                params(&[
                    ("field", "/op".into()),
                    (
                        "allowed",
                        variants
                            .iter()
                            .map(|v| v["properties"]["op"]["const"].clone())
                            .collect::<Vec<_>>()
                            .into(),
                    ),
                ]),
            )
        })?;
    let properties = variant["properties"]
        .as_object()
        .ok_or_else(|| type_error("", "object"))?;
    for field in object.keys() {
        if !properties.contains_key(field) {
            return Err(Error::invalid(
                MessageCode::InputUnknownField,
                params(&[
                    ("field", pointer(field).into()),
                    (
                        "allowed",
                        properties.keys().cloned().collect::<Vec<_>>().into(),
                    ),
                ]),
            ));
        }
    }
    if let Some(required) = variant["required"].as_array() {
        for field in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(field) {
                return Err(missing(&pointer(field)));
            }
        }
    }
    for (field, item) in object {
        if let Some(property) = properties.get(field) {
            check_type(item, property, &pointer(field))?;
        }
    }
    // Deserialize original bytes: from_value would silently accept duplicate fields.
    serde_json::from_str(raw).map_err(|source: serde_json::Error| {
        Error::from(source).with_params(params(&[("field", "".into())]))
    })
}
fn missing(field: &str) -> Error {
    Error::invalid(
        MessageCode::InputMissingField,
        params(&[("field", field.into())]),
    )
}
fn type_error(field: &str, expected: &str) -> Error {
    Error::invalid(
        MessageCode::InputJson,
        params(&[
            ("field", field.into()),
            ("expected", expected.into()),
            ("detail", crate::messages::JSON_TYPE_DETAIL.into()),
        ]),
    )
}
fn check_type(value: &Value, schema: &Value, field: &str) -> Result<(), Error> {
    let matches = |kind: &str| match kind {
        "null" => value.is_null(),
        "string" => value.is_string(),
        "integer" => value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    };
    if let Some(kind) = schema["type"].as_str() {
        if !matches(kind) {
            return Err(type_error(field, kind));
        }
    } else if let Some(kinds) = schema["type"].as_array()
        && !kinds.iter().filter_map(Value::as_str).any(matches)
    {
        return Err(type_error(
            field,
            &kinds
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" or "),
        ));
    }
    if let Some(items) = value.as_array() {
        for (i, item) in items.iter().enumerate() {
            check_type(item, &schema["items"], &format!("{field}/{i}"))?;
        }
    }
    Ok(())
}
