use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn r16_type_errors_name_the_field_and_expected_type() -> Result<(), Box<dyn std::error::Error>> {
    for (request, field, expected) in [
        (r#"{"op":"read","input":"in.txt","max_chars":"ten"}"#, "max_chars", "integer"),
        (r#"{"op":"read","input":"in.txt","locators":"yes"}"#, "locators", "boolean"),
        (r#"{"op":"merge","inputs":["in.pdf",7],"output":"out.pdf"}"#, "inputs[1]", "string"),
        (r#"{"op":"convert","input":false,"output":"out.pdf"}"#, "input", "string"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_docs")).arg(request).output()?;
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["summary_code"], "input.json");
        let pointer = format!("/{}",field.replace("[1]", "/1"));
        assert_eq!(value["summary_params"]["field"], pointer, "{value}");
        assert_eq!(value["summary_params"]["expected"], expected, "{value}");
        assert!(value.get("error").is_none());
    }
    Ok(())
}

#[test]
fn error_wire_shape_matches_library_and_has_field_or_syntax_location()
-> Result<(), Box<dyn std::error::Error>> {
    for (request, code, field) in [
        ("garbage", "input.json", None),
        ("{", "input.json", None),
        ("{}", "input.missing_field", Some("op")),
        (r#"{"op":"read"}"#, "input.missing_field", Some("input")),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_docs"))
            .arg(request)
            .output()?;
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        let keys: Vec<_> = value
            .as_object()
            .ok_or("object")?
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "ok",
                "summary",
                "summary_code",
                "summary_params",
                "warnings"
            ]
        );
        assert_eq!(value["summary_code"], code);
        assert_eq!(value["warnings"], serde_json::json!([]));
        if let Some(field) = field {
            assert_eq!(value["summary_params"]["field"], format!("/{field}"));
        } else {
            assert!(
                value["summary_params"]["line"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
            );
            assert!(
                value["summary_params"]["column"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
            );
            let error = serde_json::from_str::<docs::Input>(request)
                .err()
                .ok_or("invalid JSON accepted")?;
            assert_eq!(serde_json::to_value(docs::Error::from(error))?, value);
        }
    }
    Ok(())
}

#[test]
fn failures_are_json_with_stable_codes_and_structured_context()
-> Result<(), Box<dyn std::error::Error>> {
    for (request, code) in [
        (
            r#"{"op":"read","input":"missing.pdf","unexpected":true}"#,
            "input.unknown_field",
        ),
        (r#"{"op":"read","input":"missing.pdf"}"#, "file.read"),
        (r#"{"op":"read","input":"../outside.pdf"}"#, "path.escapes"),
        (r#"{"op":"read","input":"/outside.pdf"}"#, "path.absolute"),
        ("{", "input.json"),
    ] {
        for stdin in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_docs"));
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
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
            let result = child.wait_with_output()?;
            assert!(!result.status.success());
            assert!(result.stderr.is_empty(), "{:?}", result.stderr);
            let value: serde_json::Value = serde_json::from_slice(&result.stdout)?;
            assert_eq!(value["ok"], false);
            assert_eq!(value["summary_code"], code);
            assert!(value["summary"].is_string());
            assert!(value["summary_params"].is_object());
            if code == "input.unknown_field" {
                assert_eq!(value["summary_params"]["field"], "/unexpected");
                let allowed = value["summary_params"]["allowed"]
                    .as_array()
                    .ok_or("allowed")?;
                assert!(allowed.contains(&serde_json::json!("input")));
                assert!(allowed.contains(&serde_json::json!("max_chars")));
                assert!(!allowed.contains(&serde_json::json!("output")));
            }
            if code == "file.read" {
                assert_eq!(value["summary_params"]["path"], "missing.pdf");
                assert_eq!(value["summary_params"]["kind"], "NotFound");
                assert!(
                    !value
                        .to_string()
                        .contains(&std::env::current_dir()?.display().to_string())
                );
            }
        }
    }
    Ok(())
}
