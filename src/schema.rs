//! Form-friendly presentation of the parser's request schema.
use serde_json::{Value, json};

pub(crate) fn build() -> Value {
    let mut schema = schemars::schema_for!(crate::Input).to_value();
    if let Some(variants) = schema["oneOf"].as_array_mut() {
        for variant in variants {
            let op = variant["properties"]["op"]["const"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let description = variant["description"].as_str().unwrap_or("").to_string();
            variant["title"] = description.clone().into();
            if let Some(properties) = variant["properties"].as_object_mut() {
                for (name, property) in properties {
                    let object = match property.as_object_mut() {
                        Some(o) => o,
                        None => continue,
                    };
                    // Optional fields are omitted, not represented by nullable form unions.
                    if let Some(types) = object.get("type").and_then(Value::as_array) {
                        let kind = types.iter().find(|v| **v != "null").cloned();
                        if let Some(kind) = kind {
                            object.insert("type".into(), kind);
                        }
                    }
                    if object.get("default").is_some_and(Value::is_null) {
                        object.remove("default");
                    }
                    object.remove("format"); // Rust integer storage widths are not UI formats.
                    let help = object
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or(&description)
                        .to_string();
                    object.insert("title".into(), help.clone().into());
                    object.insert("description".into(), help.clone().into());
                    if name == "inputs" {
                        property["x-file"] = "input".into();
                        property["x-accept"] = json!(["pdf"]);
                    }
                    let target = if name == "inputs" {
                        property["items"]["title"] = "Input path".into();
                        property["items"]["description"] = help.into();
                        &mut property["items"]
                    } else {
                        property
                    };
                    match name.as_str() {
                        "input" | "inputs" => {
                            target["x-file"] = "input".into();
                            target["x-accept"] = match op.as_str() {
                                "merge" | "split" => json!(["pdf"]),
                                "convert" | "replace" => {
                                    json!(["pdf", "docx", "md", "txt", "html"])
                                }
                                _ => json!([
                                    "pdf", "docx", "md", "txt", "html", "doc", "rtf", "odt", "odp",
                                    "pptx", "xlsx", "ods", "csv", "epub", "xmind", "mhtml", "png",
                                    "jpg", "webp", "tiff"
                                ]),
                            };
                        }
                        "output" => {
                            target["x-file"] = "output".into();
                            target["x-formats"] = match op.as_str() {
                                "merge" | "split" => json!(["pdf"]),
                                "create" => json!(["pdf", "docx"]),
                                "replace" => json!(["pdf", "docx", "md", "txt", "html"]),
                                _ => json!(["pdf", "docx", "md", "txt"]),
                            };
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    schema
}
