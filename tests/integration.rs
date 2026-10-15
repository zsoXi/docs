//! Public integration metadata and validation, independent of English diagnostics.
mod support;
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};
use support::*;

fn cli(dir: &Dir, request: &str, stdin: bool) -> Result<(bool, Value)> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_docs"));
    command
        .current_dir(&dir.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin {
        command.stdin(Stdio::piped());
    } else {
        command.arg(request);
    }
    let mut child = command.spawn()?;
    if stdin {
        child
            .stdin
            .take()
            .ok_or("stdin")?
            .write_all(request.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    assert!(out.stderr.is_empty(), "{out:?}");
    Ok((out.status.success(), serde_json::from_slice(&out.stdout)?))
}

#[test]
fn catalog_cli_and_renderer_are_consistent() -> Result {
    let dir = Dir::new()?;
    let (ok, catalog) = cli(&dir, "--messages", false)?;
    assert!(ok);
    assert_eq!(catalog, docs::messages::catalog());
    assert_eq!(catalog["tool"], docs::NAME);
    assert_eq!(catalog["version"], env!("CARGO_PKG_VERSION"));
    let entries = catalog["messages"].as_array().ok_or("messages")?;
    assert_eq!(entries.len(), docs::MessageCode::ALL.len());
    for (&code, entry) in docs::MessageCode::ALL.iter().zip(entries) {
        assert_eq!(entry["code"], code.as_str());
        assert!(
            ["summary", "warning", "error", "label"]
                .contains(&entry["kind"].as_str().ok_or("kind")?)
        );
        assert_eq!(entry["template"], docs::messages::template(code, "en"));
        assert_eq!(
            entry["params"],
            json!(docs::messages::required_params(code, "en"))
        );
    }
    Ok(())
}

#[test]
fn ocr_setup_errors_keep_safe_context_and_original_cause() -> Result {
    use std::error::Error as _;
    let dir = Dir::new()?;
    image::RgbImage::from_pixel(8, 8, image::Rgb([255, 255, 255])).save(dir.0.join("scan.png"))?;
    let empty = dir.0.join("empty-models");
    std::fs::create_dir(&empty)?;
    let request = serde_json::from_value(json!({"op":"read","input":"scan.png"}))?;
    let opts = docs::Options {
        ocr_models: Some(empty),
        ..docs::Options::new(&dir.0)
    };
    let err = docs::run(&request, &opts)
        .err()
        .ok_or("missing models accepted")?;
    assert_eq!(err.code(), docs::MessageCode::ReadOcrModelMissing);
    assert_eq!(err.params()["stage"], "model");
    assert!(err.params()["asset"].is_string());
    assert!(err.source().is_some());
    assert!(!serde_json::to_string(&err)?.contains(&dir.0.display().to_string()));
    if let Some(models) = std::env::var_os("DOCS_TEST_OCR_MODELS") {
        let opts = docs::Options {
            ocr_models: Some(models.into()),
            ocr_runtime: Some(dir.0.join("missing-runtime.so")),
            ..docs::Options::new(&dir.0)
        };
        let err = docs::run(&request, &opts)
            .err()
            .ok_or("missing runtime accepted")?;
        assert_eq!(err.code(), docs::MessageCode::ReadOcrRuntimeMissing);
        assert_eq!(err.params()["stage"], "runtime");
        assert_eq!(err.params()["kind"], "NotFound");
        assert!(err.source().is_some());
        assert!(!serde_json::to_string(&err)?.contains(&dir.0.display().to_string()));
    }
    Ok(())
}
fn walk(node: &Value, path: &str) {
    assert!(node.is_object(), "schema object at {path}");
    let Some(object) = node.as_object() else {
        return;
    };
    const KEYS: &[&str] = &[
        "type",
        "enum",
        "const",
        "default",
        "title",
        "description",
        "properties",
        "required",
        "items",
        "additionalProperties",
        "format",
        "pattern",
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "minItems",
        "maxItems",
        "x-file",
        "x-accept",
        "x-formats",
    ];
    for key in object.keys() {
        assert!(
            KEYS.contains(&key.as_str()),
            "forbidden keyword {path}/{key}"
        );
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (key, value) in properties {
            assert!(
                value["title"].as_str().is_some_and(|s| !s.is_empty()),
                "{path}/{key}"
            );
            assert!(
                value["description"].as_str().is_some_and(|s| !s.is_empty()),
                "{path}/{key}"
            );
            if ["input", "inputs", "output"].contains(&key.as_str()) {
                assert_eq!(
                    value["x-file"],
                    if key == "output" { "output" } else { "input" }
                );
                assert!(
                    value[if key == "output" {
                        "x-formats"
                    } else {
                        "x-accept"
                    }]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
                );
            }
            walk(value, &format!("{path}/{key}"));
        }
    }
    if let Some(items) = object.get("items") {
        if node["x-file"] == "input" {
            assert_eq!(items["x-file"], "input");
            assert!(items["x-accept"].is_array());
        }
        walk(items, &format!("{path}/items"));
    }
    if let Some(value) = object.get("additionalProperties").filter(|v| v.is_object()) {
        walk(value, &format!("{path}/additionalProperties"));
    }
}

#[test]
fn schema_is_a_complete_form_contract() -> Result {
    let schema = docs::schema();
    let dir = Dir::new()?;
    assert_eq!(cli(&dir, "--schema", false)?.1, schema);
    for variant in schema["oneOf"].as_array().ok_or("variants")? {
        assert!(variant["title"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(
            variant["description"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
        walk(variant, "op");
    }
    if let Some(defs) = schema["$defs"].as_object() {
        for (name, value) in defs {
            walk(value, name);
        }
    }
    Ok(())
}

#[test]
fn validation_pointers_envelopes_and_stdin_argument_parity() -> Result {
    let dir = Dir::new()?;
    std::fs::write(dir.0.join("input.txt"), "Approved balance $1,234.50")?;
    for request in [
        json!({"op":"read","input":"input.txt"}),
        json!({"op":"replace","input":"input.txt","output":"out.txt","find":"Approved","replace":"Reviewed"}),
        json!({"op":"read","input":"input.txt","a/b~c":1}),
        json!({"op":"merge","inputs":["input.pdf",false],"output":"out.pdf"}),
        json!({"op":"read"}),
        json!({"op":"other"}),
        json!({"op":"replace","input":"input.txt","output":"out.txt","find":"","replace":"x"}),
        json!({"op":"merge","inputs":[],"output":"out.pdf"}),
        json!({"op":"read","input":"input.txt","pages":"1"}),
    ] {
        let arg = cli(&dir, &request.to_string(), false)?;
        let stdin = cli(&dir, &request.to_string(), true)?;
        assert_eq!(arg, stdin);
        assert!(arg.1["warnings"].is_array(), "{:?}", arg.1);
        if !arg.0 {
            let pointer = arg.1["summary_params"]["field"].as_str().ok_or("field")?;
            assert!(pointer.is_empty() || pointer.starts_with('/'), "{pointer}");
            if request.get("a/b~c").is_some() {
                assert_eq!(pointer, "/a~1b~0c");
            }
            if request["op"] == "other" {
                assert!(arg.1["summary_params"]["allowed"].is_array());
            }
            assert_eq!(arg.1.as_object().ok_or("envelope")?.len(), 5);
        }
    }
    assert_eq!(cli(&dir, "--help", false)?.1["warnings"], json!([]));
    // Nullable legacy input remains accepted, without nullable UI unions.
    docs::parse_input(r#"{"op":"read","input":"input.txt","range":null}"#)?;
    Ok(())
}
