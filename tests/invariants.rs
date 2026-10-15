//! Seeded class-level regressions. Failure messages include a replay seed.
mod support;
use serde_json::json;
use std::process::Command;
use support::*;
const SEED: u64 = 0x5eed_2026_0331;
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[test]
fn i1_two_hundred_numeric_documents_roundtrip_exactly() -> Result {
    eprintln!("I1 seed={SEED:x}");
    let dir = Dir::new()?;
    let mut state = SEED;
    for document in 0..200 {
        let n = next(&mut state) % 9000 + 1000;
        let tokens = vec![
            format!("{n}"),
            format!("{n}.50"),
            format!("-{n}.25"),
            format!("({n})"),
            format!("-{n}.5%"),
            format!("${n}.50"),
            format!("${},234.50", n % 9 + 1),
            format!("({},400)", n % 9 + 1),
            format!("2026-{:02}-{:02}", n % 12 + 1, n % 28 + 1),
            format!("1234567890123456{n}"),
        ];
        // Rotate each numeric class through paragraph, list and table locations.
        let mut source = String::from("# Financial record\n\n");
        for (i, token) in tokens.iter().enumerate() {
            source.push_str(&match (i + document) % 3 {
                0 => format!("Paragraph value {token} confirmed.\n\n"),
                1 => format!("- List value {token} confirmed\n\n"),
                _ => format!("| Entry | Value |\n| --- | --- |\n| Balance | {token} |\n\n"),
            });
        }
        for format in ["docx", "pdf"] {
            let name = format!("numbers.{format}");
            dir.request(json!({"op":"create","markdown":source,"output":name}))
                .map_err(|e| format!("seed={SEED:x}, case={document}, {format}: {e}"))?;
            let out = dir.read(&name)?;
            let text = out.text.as_deref().ok_or("text")?;
            for token in &tokens {
                let numeric = |c: char| c.is_ascii_digit() || "$€£¥.,%()+-/".contains(c);
                let actual = text
                    .match_indices(token.as_str())
                    .filter(|(at, _)| {
                        let before = text[..*at].chars().next_back();
                        let after = text[*at + token.len()..].chars().next();
                        !before.is_some_and(numeric) && !after.is_some_and(numeric)
                    })
                    .count();
                let warned = out.warnings.iter().any(|w| {
                    w.code == "read.numbers_unverified"
                        && w.params["token"] == *token
                        && w.params["page"].as_u64().is_some_and(|n| n > 0)
                        && w.params["line"].as_u64().is_some_and(|n| n > 0)
                });
                assert!(
                    actual == 1 || warned,
                    "I1 seed={SEED:x} case={document} format={format} token={token:?}, actual={actual}: {text}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn i2_creation_is_deterministic_after_unrelated_requests_and_across_processes() -> Result {
    eprintln!("I2 seed={SEED:x}");
    let dir = Dir::new()?;
    dir.report()?;
    for format in ["docx", "pdf"] {
        let request = json!({"op":"create","markdown":REPORT,"output":format!("stable.{format}")});
        let first = dir.repeat(request.clone())?;
        let path = first.output.ok_or("path")?;
        let bytes = std::fs::read(dir.0.join(&path))?;
        dir.request(json!({"op":"create","markdown":"# Other\n\n[link](https://example.org/other)\n\n<img src=\"chart.png\" width=\"11\" height=\"12\">","output":format!("other.{format}")}))?;
        let output = dir.request(request.clone())?;
        assert_eq!(bytes, std::fs::read(dir.0.join(&path))?, "I2 seed={SEED:x}");
        let child = Command::new(env!("CARGO_BIN_EXE_docs"))
            .arg(request.to_string())
            .current_dir(&dir.0)
            .output()?;
        assert!(child.status.success(), "{child:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&child.stdout)?,
            serde_json::to_value(output)?
        );
        assert_eq!(
            bytes,
            std::fs::read(dir.0.join(path))?,
            "I2 process bytes seed={SEED:x}"
        );
        // Cross counter digit boundaries: package/resource order must not depend
        // on whether previous requests allocated IDs 9, 10, 99 or 100.
        let mut state = SEED;
        for _ in 0..4 {
            let count = (next(&mut state) % 12 + 10) as usize;
            let markdown=(0..count).map(|i|format!("[Audit {i}](https://example.org/audit/{i})\n\n<img src=\"chart.png\" width=\"12\" height=\"4\">\n\n")).collect::<String>();
            dir.repeat(
                json!({"op":"create","markdown":markdown,"output":format!("many.{format}")}),
            )?;
        }
    }
    Ok(())
}

#[test]
fn i3_malformed_input_generator_returns_stable_json_without_crash_or_hang() -> Result {
    eprintln!("I3 seed={SEED:x}");
    let dir = Dir::new()?;
    dir.native_docx()?;
    dir.native_pdf()?;
    let mut state = SEED;
    let mut cases = Vec::new();
    for format in ["docx", "pdf"] {
        let original = std::fs::read(dir.0.join(format!("report.{format}")))?;
        for i in 0..24 {
            let length = 1 + (next(&mut state) as usize % 60).min(original.len() - 1);
            // Retain only a short prefix; mutate one random byte in alternate cases.
            let mut bytes = original[..length].to_vec();
            if i % 2 == 1 {
                let at = next(&mut state) as usize % bytes.len();
                bytes[at] ^= 0xff;
            }
            cases.push((format!("broken-{i}.{format}"), bytes));
        }
    }
    cases.push((
        "renamed.docx".into(),
        std::fs::read(dir.0.join("report.pdf"))?,
    ));
    cases.push((
        "renamed.pdf".into(),
        std::fs::read(dir.0.join("report.docx"))?,
    ));
    cases.extend([
        ("empty.pdf".into(), Vec::new()),
        ("empty.docx".into(), Vec::new()),
        ("wrong.docx".into(), b"This is not a package".to_vec()),
        ("wrong.pdf".into(), b"This is not a PDF".to_vec()),
        ("invalid.md".into(), vec![0xff, 0xfe]),
        (
            "deep.md".into(),
            format!("{}nested", " >".repeat(1000)).into_bytes(),
        ),
        (
            "deep-html.md".into(),
            format!("{}nested{}", "<div>".repeat(1000), "</div>".repeat(1000)).into_bytes(),
        ),
    ]);
    for (i, (name, bytes)) in cases.into_iter().enumerate() {
        std::fs::write(dir.0.join(&name), bytes)?;
        let request = json!({"op":"read","input":name});
        let mut expected = None;
        for _ in 0..2 {
            let child = Command::new("timeout")
                .args(["12", env!("CARGO_BIN_EXE_docs"), &request.to_string()])
                .current_dir(&dir.0)
                .output()?;
            assert_eq!(
                child.status.code(),
                Some(1),
                "I3 seed={SEED:x} case={i} {name}: {child:?}"
            );
            let value: serde_json::Value = serde_json::from_slice(&child.stdout)
                .map_err(|e| format!("seed={SEED:x} case={i}: {e}: {child:?}"))?;
            assert_eq!(value["ok"], false, "seed={SEED:x} {name}");
            assert!(
                value["summary_code"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            );
            assert_eq!(value["warnings"], json!([]));
            if let Some(expected) = &expected {
                assert_eq!(expected, &value, "seed={SEED:x} {name}");
            } else {
                expected = Some(value);
            }
        }
    }
    let sparse = std::fs::File::create(dir.0.join("oversize.pdf"))?;
    sparse.set_len(101 * 1024 * 1024)?;
    let child = Command::new("timeout")
        .args([
            "12",
            env!("CARGO_BIN_EXE_docs"),
            r#"{"op":"read","input":"oversize.pdf"}"#,
        ])
        .current_dir(&dir.0)
        .output()?;
    assert_eq!(child.status.code(), Some(1), "seed={SEED:x}: {child:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&child.stdout)?["summary_code"],
        "file.too_large"
    );
    let opts = docs::Options {
        max_input_bytes: 1,
        ..docs::Options::new(&dir.0)
    };
    let input = serde_json::from_value(json!({"op":"read","input":"report.pdf"}))?;
    let error = docs::run(&input, &opts).err().ok_or("oversize accepted")?;
    assert_eq!(
        serde_json::to_value(error)?["summary_code"],
        "file.too_large"
    );
    Ok(())
}
