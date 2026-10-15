use super::super::raster::{Scratch, decode};
use super::{Result, read, save, text};
use crate::{Input, Options, run};
use serde_json::json;

#[test]
fn created_pdf_reads_as_equivalent_markdown() -> Result {
    let dir = Scratch::new()?;
    let source = "# Report\n\nBefore **bold** after and *italic*.\n\n- First item\n- Second item\n\n| Name | Value |\n| --- | --- |\n| Alpha | 42 |\n\nFinal paragraph.";
    run(
        &Input::Create {
            markdown: source.into(),
            output: "source.pdf".into(),
        },
        &Options::new(&dir.0),
    )?;
    let out = read(&dir, "source.pdf", json!({}))?;
    assert_eq!(text(&out).trim(), source);
    Ok(())
}

#[test]
fn repeated_tight_pdf_list_items_remain_tight() -> Result {
    let dir = Scratch::new()?;
    let source =
        "# Report\n\nBody paragraph.\n\n- Repeated item\n- Repeated item\n\nFinal paragraph.";
    run(
        &Input::Create {
            markdown: source.into(),
            output: "repeat.pdf".into(),
        },
        &Options::new(&dir.0),
    )?;
    assert_eq!(text(&read(&dir, "repeat.pdf", json!({}))?).trim(), source);
    Ok(())
}

#[test]
fn ocr_paragraph_list_and_three_by_three_table() -> Result {
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        eprintln!("SKIP: set ORT_DYLIB_PATH to run OCR model tests");
        return Ok(());
    }
    let Some(models) = std::env::var_os("DOCS_TEST_OCR_MODELS") else {
        eprintln!("SKIP: set DOCS_TEST_OCR_MODELS to run OCR model tests");
        return Ok(());
    };
    let dir = Scratch::new()?;
    let opts = Options {
        ocr_models: Some(models.into()),
        ..Options::new(&dir.0)
    };
    save(&dir, "font.ttf", crate::pdf::load_font(None)?)?;
    decode(
        &dir,
        "magick",
        &[
            "-size",
            "1600x700",
            "xc:white",
            "-font",
            "/work/font.ttf",
            "-pointsize",
            "40",
            "-fill",
            "black",
            "-annotate",
            "+40+65",
            "Regional sales report.",
            "-annotate",
            "+40+145",
            "1. First item",
            "-annotate",
            "+40+205",
            "2. Second item",
            "-annotate",
            "+40+325",
            "Region",
            "-annotate",
            "+540+325",
            "Q2",
            "-annotate",
            "+1040+325",
            "Q3",
            "-annotate",
            "+40+405",
            "North",
            "-annotate",
            "+540+405",
            "19",
            "-annotate",
            "+1040+405",
            "21",
            "-annotate",
            "+40+485",
            "South",
            "-annotate",
            "+540+485",
            "23",
            "-annotate",
            "+1040+485",
            "28",
            "/work/layout.png",
        ],
    )?;
    let request: Input = serde_json::from_value(json!({"op":"read", "input":"layout.png"}))?;
    let out = run(&request, &opts)?;
    let value = text(&out);
    assert!(value.contains("Regional sales report."), "{value}");
    assert!(value.contains("1. First item\n2. Second item"), "{value}");
    assert!(
        value.contains(
            "| Region | Q2 | Q3 |\n| --- | --- | --- |\n| North | 19 | 21 |\n| South | 23 | 28 |"
        ),
        "{value}"
    );
    assert!(out.warnings.iter().any(|w| w.code == "read.ocr"));
    Ok(())
}
