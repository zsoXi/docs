//! End-to-end tests for the `docs` operations.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use docs::{Input, Options, Output, run};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> std::io::Result<Self> {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "docs-test-{}-{}-{}",
            std::process::id(),
            unique,
            label
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn options(&self) -> Options {
        Options::new(self.path.clone())
    }

    fn write(&self, name: &str, content: &[u8]) -> std::io::Result<()> {
        std::fs::write(self.path.join(name), content)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn read(dir: &TempDir, name: &str) -> Result<Output, docs::Error> {
    run(
        &Input::Read {
            input: name.to_string(),
            max_chars: 20_000,
            locators: false,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        &dir.options(),
    )
}

fn text_of(output: &Output) -> &str {
    output.text.as_deref().unwrap_or("")
}

fn convert(dir: &TempDir, from: &str, to: &str) -> Result<Output, docs::Error> {
    run(
        &Input::Convert {
            input: from.to_string(),
            output: to.to_string(),
        },
        &dir.options(),
    )
}

#[cfg(unix)]
#[test]
fn rejects_new_output_through_symlinked_parent() -> Result<(), Box<dyn std::error::Error>> {
    let base = TempDir::new("symlink-base")?;
    let outside = TempDir::new("symlink-outside")?;
    std::os::unix::fs::symlink(outside.path(), base.path().join("escape"))?;
    let result = run(
        &Input::Create {
            markdown: "Private".into(),
            output: "escape/new/out.docx".into(),
        },
        &base.options(),
    );
    // Links are forbidden even when they stay inside base_dir. The revised
    // file contract diagnoses the link itself, without resolving its target.
    assert_eq!(
        result.err().ok_or("accepted symlink")?.code().as_str(),
        "path.symlink"
    );
    assert!(!outside.path().join("new/out.docx").exists());
    Ok(())
}

const NOTE: &str = "# Title\n\nHello **world** and *italic*.\n\n- one\n- two\n\n1. first\n2. second\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n[link](https://example.com)\n\nZażółć gęślą jaźń.\n";

#[test]
fn read_reports_words_and_truncates() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("read")?;
    dir.write("notes.txt", b"one two three four five")?;

    let output = run(
        &Input::Read {
            input: "notes.txt".to_string(),
            max_chars: 7,
            locators: false,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        &dir.options(),
    )?;
    assert!(output.summary.contains("5 word(s)"));
    assert!(text_of(&output).chars().count() <= 8);
    assert!(!output.warnings.is_empty());
    Ok(())
}

#[test]
fn read_pdf_reports_pages() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("readpdf")?;
    let output = run(
        &Input::Create {
            markdown: "# Page\n\nSome text on the page.".to_string(),
            output: "page.pdf".to_string(),
        },
        &dir.options(),
    )?;
    assert!(output.summary.contains("created"));
    let output = read(&dir, "page.pdf")?;
    assert!(output.summary.contains("1 page(s)"));
    assert!(text_of(&output).contains("Some text"));
    Ok(())
}

#[test]
fn create_docx_keeps_markdown_content() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("create")?;
    run(
        &Input::Create {
            markdown: NOTE.to_string(),
            output: "out.docx".to_string(),
        },
        &dir.options(),
    )?;
    let output = read(&dir, "out.docx")?;
    let text = text_of(&output);
    assert!(text.contains("Title"));
    assert!(text.contains("world"));
    assert!(text.contains("one"));
    assert!(text.contains("first"));
    assert!(text.contains("Zażółć gęślą jaźń."));
    Ok(())
}

#[test]
fn replace_plain_text() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("replacetxt")?;
    dir.write("in.txt", b"Hello world. Hello again.")?;
    let output = run(
        &Input::Replace {
            input: "in.txt".to_string(),
            output: "out.txt".to_string(),
            find: "Hello".to_string(),
            replace: "Bye".to_string(),
            all: true,
        },
        &dir.options(),
    )?;
    assert!(output.summary.contains("2 occurrence"));
    let written = std::fs::read_to_string(dir.path().join("out.txt"))?;
    assert_eq!(written, "Bye world. Bye again.");
    Ok(())
}

#[test]
fn replace_in_docx_replaces_text() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("replacedocx")?;
    run(
        &Input::Create {
            markdown: "Zażółć gęślą jaźń.".to_string(),
            output: "in.docx".to_string(),
        },
        &dir.options(),
    )?;
    run(
        &Input::Replace {
            input: "in.docx".to_string(),
            output: "out.docx".to_string(),
            find: "Zażółć".to_string(),
            replace: "Zamiast".to_string(),
            all: true,
        },
        &dir.options(),
    )?;
    let output = read(&dir, "out.docx")?;
    assert!(text_of(&output).contains("Zamiast gęślą jaźń."));
    assert!(!text_of(&output).contains("Zażółć"));
    Ok(())
}

#[test]
fn replace_in_pdf_replaces_text() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("replacepdf")?;
    run(
        &Input::Create {
            markdown: "Zażółć gęślą jaźń.".to_string(),
            output: "in.pdf".to_string(),
        },
        &dir.options(),
    )?;
    run(
        &Input::Replace {
            input: "in.pdf".to_string(),
            output: "out.pdf".to_string(),
            find: "Zażółć".to_string(),
            replace: "Zamiast".to_string(),
            all: true,
        },
        &dir.options(),
    )?;
    let output = read(&dir, "out.pdf")?;
    assert!(text_of(&output).contains("Zamiast gęślą jaźń."));
    assert!(!text_of(&output).contains("Zażółć"));
    Ok(())
}

#[test]
fn replace_in_markdown_and_html() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("replacemisc")?;
    dir.write("in.md", b"# Hello\n\nHello world.")?;
    dir.write("in.html", b"<p>Hello world.</p>")?;

    for (input, output) in [("in.md", "out.md"), ("in.html", "out.html")] {
        let result = run(
            &Input::Replace {
                input: input.to_string(),
                output: output.to_string(),
                find: "Hello".to_string(),
                replace: "Bye".to_string(),
                all: true,
            },
            &dir.options(),
        )?;
        assert!(result.summary.contains("2 occurrence") || result.summary.contains("1 occurrence"));
        let written = std::fs::read_to_string(dir.path().join(output))?;
        assert!(written.contains("Bye"));
        assert!(!written.contains("Hello"));
    }
    Ok(())
}

#[test]
fn merge_adds_pages_and_split_extracts() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("mergesplit")?;

    let long = {
        let mut markdown = String::from("# Long\n\n");
        for i in 0..80 {
            markdown.push_str(&format!("Paragraph number {i} with some more text.\n\n"));
        }
        markdown
    };
    run(
        &Input::Create {
            markdown: long,
            output: "long.pdf".to_string(),
        },
        &dir.options(),
    )?;
    run(
        &Input::Create {
            markdown: "# Short\n\nOne page only.".to_string(),
            output: "short.pdf".to_string(),
        },
        &dir.options(),
    )?;

    let short = read(&dir, "short.pdf")?;
    let long = read(&dir, "long.pdf")?;
    let short_pages = pages_of(&short)?;
    let long_pages = pages_of(&long)?;

    run(
        &Input::Merge {
            inputs: vec!["short.pdf".to_string(), "long.pdf".to_string()],
            output: "merged.pdf".to_string(),
        },
        &dir.options(),
    )?;
    let merged = read(&dir, "merged.pdf")?;
    assert_eq!(pages_of(&merged)?, short_pages + long_pages);

    run(
        &Input::Split {
            input: "merged.pdf".to_string(),
            pages: "2".to_string(),
            output: "page2.pdf".to_string(),
        },
        &dir.options(),
    )?;
    let page2 = read(&dir, "page2.pdf")?;
    assert_eq!(pages_of(&page2)?, 1);
    Ok(())
}

fn pages_of(output: &Output) -> Result<usize, Box<dyn std::error::Error>> {
    let summary = &output.summary;
    let start = summary.find("read ").ok_or("no 'read' in summary")?;
    let rest = &summary[start + 5..];
    let end = rest.find(" page(s)").ok_or("no page count")?;
    Ok(rest[..end].parse::<usize>()?)
}

/// A one-pixel PNG, enough to exercise local image embedding.
const ONE_PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

#[test]
fn convert_refuses_to_overwrite_input() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("overwrite")?;
    dir.write("note.md", b"# Title\n")?;
    let result = convert(&dir, "note.md", "note.md");
    assert!(matches!(result, Err(docs::Error::Invalid(_))));
    Ok(())
}

#[test]
fn markdown_to_pdf_embeds_a_local_image() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("mdimage")?;
    dir.write("pic.png", ONE_PIXEL_PNG)?;
    dir.write("note.md", b"# Title\n\n![a pixel](pic.png)\n")?;
    let output = convert(&dir, "note.md", "note.pdf")?;
    assert!(output.bytes.unwrap_or(0) > 0);
    assert!(
        !output
            .warnings
            .iter()
            .any(|warning| warning.message.contains("could not be read")),
        "{:?}",
        output.warnings
    );
    Ok(())
}

#[test]
fn roundtrip_md_pdf() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("rtmdpdf")?;
    dir.write("note.md", NOTE.as_bytes())?;
    convert(&dir, "note.md", "note.pdf")?;
    let output = read(&dir, "note.pdf")?;
    assert!(text_of(&output).contains("Zażółć gęślą jaźń."));
    assert!(text_of(&output).contains("world"));
    Ok(())
}

#[test]
fn roundtrip_md_docx() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("rtmddocx")?;
    dir.write("note.md", NOTE.as_bytes())?;
    convert(&dir, "note.md", "note.docx")?;
    let output = read(&dir, "note.docx")?;
    assert!(text_of(&output).contains("Zażółć gęślą jaźń."));
    assert!(text_of(&output).contains("world"));
    Ok(())
}

#[test]
fn roundtrip_docx_pdf() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("rtdocxpdf")?;
    dir.write("note.md", NOTE.as_bytes())?;
    convert(&dir, "note.md", "note.docx")?;
    convert(&dir, "note.docx", "note.pdf")?;
    let output = read(&dir, "note.pdf")?;
    assert!(text_of(&output).contains("Zażółć gęślą jaźń."));
    Ok(())
}

#[test]
fn roundtrip_pdf_docx() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("rtpdfdocx")?;
    dir.write("note.md", NOTE.as_bytes())?;
    convert(&dir, "note.md", "note.pdf")?;
    convert(&dir, "note.pdf", "note.docx")?;
    let output = read(&dir, "note.docx")?;
    assert!(text_of(&output).contains("Zażółć gęślą jaźń."));
    Ok(())
}

#[test]
fn text_conversions() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("textconv")?;
    dir.write("in.txt", b"plain text body")?;
    convert(&dir, "in.txt", "out.pdf")?;
    convert(&dir, "in.txt", "out.docx")?;
    assert!(text_of(&read(&dir, "out.pdf")?).contains("plain text"));
    assert!(text_of(&read(&dir, "out.docx")?).contains("plain text"));
    Ok(())
}

#[test]
fn html_conversions() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("htmlconv")?;
    dir.write(
        "page.html",
        b"<html><body><h1>Head</h1><p>Some <b>bold</b> text.</p></body></html>",
    )?;
    convert(&dir, "page.html", "page.pdf")?;
    convert(&dir, "page.html", "page.docx")?;
    convert(&dir, "page.html", "page.md")?;
    assert!(text_of(&read(&dir, "page.pdf")?).contains("bold"));
    assert!(text_of(&read(&dir, "page.docx")?).contains("bold"));
    let markdown = std::fs::read_to_string(dir.path().join("page.md"))?;
    assert!(markdown.contains("Head"));
    Ok(())
}

#[test]
fn rejects_parent_escape() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("escape")?;
    let result = run(
        &Input::Read {
            input: "../outside.txt".to_string(),
            max_chars: 100,
            locators: false,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        &dir.options(),
    );
    assert!(matches!(result, Err(docs::Error::PathEscapes { .. })));
    Ok(())
}

#[test]
fn rejects_absolute_path() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("absolute")?;
    let result = run(
        &Input::Read {
            input: "/etc/passwd".to_string(),
            max_chars: 100,
            locators: false,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        &dir.options(),
    );
    assert!(matches!(result, Err(docs::Error::AbsolutePath { .. })));
    Ok(())
}

#[test]
fn rejects_unknown_field() {
    let parsed = serde_json::from_str::<Input>(r#"{"op":"read","input":"x.txt","bogus":1}"#);
    assert!(parsed.is_err());
}

#[test]
fn rejects_too_large_input() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("toolarge")?;
    dir.write("big.txt", b"0123456789")?;
    let options = Options {
        base_dir: dir.path().to_path_buf(),
        max_input_bytes: 4,
        font_path: None,
        ocr_models: None,
        locale: "en".to_string(),
        ..Options::default()
    };
    let result = run(
        &Input::Read {
            input: "big.txt".to_string(),
            max_chars: 100,
            locators: false,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        &options,
    );
    assert!(matches!(result, Err(docs::Error::TooLarge { .. })));
    Ok(())
}

#[test]
fn rejects_unsupported_conversion() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("unsupported")?;
    run(
        &Input::Create {
            markdown: "hello".to_string(),
            output: "a.pdf".to_string(),
        },
        &dir.options(),
    )?;
    let result = convert(&dir, "a.pdf", "b.pdf");
    assert!(matches!(
        result,
        Err(docs::Error::UnsupportedConversion { .. })
    ));
    Ok(())
}

#[test]
fn schema_is_an_object() {
    let schema = docs::schema();
    assert!(schema.is_object());
}

#[test]
fn missing_font_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("nofont")?;
    let options = Options {
        base_dir: dir.path().to_path_buf(),
        max_input_bytes: 1024,
        font_path: Some(dir.path().join("does-not-exist.ttf")),
        ocr_models: None,
        locale: "en".to_string(),
        ..Options::default()
    };
    let result = run(
        &Input::Create {
            markdown: "hello".to_string(),
            output: "a.pdf".to_string(),
        },
        &options,
    );
    assert!(matches!(result, Err(docs::Error::FontRead { .. })));
    Ok(())
}

#[test]
fn warnings_and_summary_carry_message_codes() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("msgcodes")?;
    dir.write("notes.txt", b"one two three four five")?;
    let output = run(
        &Input::Read {
            input: "notes.txt".to_string(),
            max_chars: 7,
            locators: false,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        &dir.options(),
    )?;
    assert_eq!(output.summary_code, "read.words");
    assert_eq!(output.summary_params["words"], serde_json::json!(5));
    assert!(output.summary.contains("5 word(s)"));

    let warning = output.warnings.first().ok_or("no warning")?;
    assert_eq!(warning.code, "read.truncated");
    assert_eq!(warning.params["max_chars"], serde_json::json!(7));
    assert!(warning.message.contains("7 characters"));

    let json = serde_json::to_value(&output)?;
    let warnings = json["warnings"].as_array().ok_or("warnings not an array")?;
    let first = warnings.first().ok_or("no warning in json")?;
    assert_eq!(first["code"], serde_json::json!("read.truncated"));
    assert!(first["message"].is_string());
    assert!(first["params"].is_object());
    assert_eq!(json["summary_code"], serde_json::json!("read.words"));
    Ok(())
}

#[test]
fn errors_carry_code_and_params() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("errcodes")?;
    let result = run(
        &Input::Read {
            input: "../outside.txt".to_string(),
            max_chars: 10,
            locators: false,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        &dir.options(),
    );
    let error = result.err().ok_or("expected an error")?;
    assert_eq!(error.code(), docs::MessageCode::PathEscapes);
    assert_eq!(error.code().as_str(), "path.escapes");
    assert_eq!(error.params()["path"], serde_json::json!("../outside.txt"));
    assert!(error.to_string().contains("escapes base_dir"));
    Ok(())
}

#[test]
fn unknown_locale_falls_back_to_english() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new("locale")?;
    dir.write("notes.txt", b"hello world")?;
    let options = Options {
        base_dir: dir.path().to_path_buf(),
        max_input_bytes: 1024,
        font_path: None,
        ocr_models: None,
        locale: "xx-unknown".to_string(),
        ..Options::default()
    };
    let output = run(
        &Input::Read {
            input: "notes.txt".to_string(),
            max_chars: 100,
            locators: false,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        &options,
    )?;
    assert!(output.summary.starts_with("read 2 word(s)"));
    Ok(())
}

#[test]
fn message_catalog_is_complete_for_every_code() {
    use docs::messages::{MessageCode, render, required_params, template};
    let mut seen = std::collections::BTreeSet::new();
    for code in MessageCode::ALL {
        assert!(
            seen.insert(code.as_str()),
            "duplicate code {}",
            code.as_str()
        );
        assert!(!template(*code, "en").trim().is_empty());
        let mut supplied = docs::messages::Params::new();
        for name in required_params(*code, "en") {
            supplied.insert(name.to_string(), serde_json::json!("x"));
        }
        let rendered = render(*code, &supplied, "en");
        assert!(
            !rendered.contains('{') && !rendered.contains('}'),
            "unfilled parameter for {}: {rendered}",
            code.as_str()
        );
    }
}
