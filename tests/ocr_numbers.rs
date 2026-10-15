//! Numeric fidelity is checked independently from CER and overall page averages.
mod support;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use support::*;

fn numbers(text: &str) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    let mut current = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() || "$€£¥.,%()+-/:".contains(ch) {
            current.push(ch);
        } else {
            let token = current.trim_matches(['.', ',', ':', '/']);
            if token.chars().any(|c| c.is_ascii_digit()) {
                *result.entry(token.into()).or_default() += 1;
            }
            current.clear();
        }
    }
    result
}
#[test]
fn i1_ocr_corpus_numbers_are_exact_unless_low_confidence() -> Result {
    const SEED: u64 = 0x5eed_2026_0331;
    eprintln!("I1 OCR seed={SEED:x}");
    let Some(models) = std::env::var_os("DOCS_TEST_OCR_MODELS") else {
        eprintln!("SKIP I1 OCR: set DOCS_TEST_OCR_MODELS, DOCS_TEST_OCR_CORPUS and ORT_DYLIB_PATH");
        return Ok(());
    };
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        eprintln!("SKIP I1 OCR: set ORT_DYLIB_PATH");
        return Ok(());
    }
    let models = std::path::PathBuf::from(models);
    let corpus = std::env::var_os("DOCS_TEST_OCR_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or(models.parent().ok_or("models")?.join("corpus"));
    let fixtures: Vec<Value> = serde_json::from_str(include_str!("fixtures/ocr-regression.json"))?;
    let dir = Dir::new()?;
    let opts = docs::Options {
        ocr_models: Some(models),
        ocr_worker: Some(env!("CARGO_BIN_EXE_docs-ocr").into()),
        ..docs::Options::new(&dir.0)
    };
    for fixture in fixtures {
        let name = std::path::Path::new(fixture["path"].as_str().ok_or("path")?)
            .file_name()
            .ok_or("name")?;
        std::fs::copy(corpus.join(name), dir.0.join(name))?;
        let request = serde_json::from_value(
            json!({"op":"read","input":name.to_str().ok_or("UTF8 name")?,"max_chars":100000}),
        )?;
        let out = docs::run(&request, &opts)?;
        assert!(
            !out.warnings.iter().any(|w| [
                "ocr.page_failed",
                "ocr.time_limit",
                "read.ocr_unavailable"
            ]
            .contains(&w.code.as_str())),
            "seed={SEED:x} {name:?}: {out:?}"
        );
        if !out.warnings.iter().any(|w| w.code == "ocr.low_confidence") {
            let expected = numbers(fixture["text"].as_str().ok_or("ground truth")?);
            let actual = numbers(out.text.as_deref().ok_or("OCR text")?);
            assert_eq!(actual, expected, "I1 OCR seed={SEED:x} {name:?}: {out:?}");
        }
    }
    Ok(())
}
