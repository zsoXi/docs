//! Geometry regressions measured against Writer's generated Markdown DOCX.
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn pdf_lines(markdown: &str) -> Result<Vec<pdfedit::PositionedLine>, Box<dyn std::error::Error>> {
    let bytes = pdf::html_to_pdf(
        &markdown::full_html(&markdown::to_html(markdown)),
        &pdf::load_font(None)?,
    )?;
    let doc = lopdf::Document::load_mem(&bytes)?;
    let mut lines = Vec::new();
    for page in doc.get_pages().into_values() {
        lines.extend(pdfedit::positioned_lines(&doc, page)?);
    }
    Ok(lines)
}

fn line<'a>(
    lines: &'a [pdfedit::PositionedLine],
    text: &str,
) -> Result<&'a pdfedit::PositionedLine, Box<dyn std::error::Error>> {
    lines
        .iter()
        .find(|l| l.text.contains(text))
        .ok_or_else(|| format!("missing {text}: {lines:?}").into())
}

#[test]
fn markdown_list_markers_share_baselines_and_hanging_indents() -> TestResult {
    let lines = pdf_lines("Before\n\n1. First\n2. Second\n   - Nested\n3. Last\n\nAfter")?;
    let before = line(&lines, "Before")?;
    let first = line(&lines, "First")?;
    let nested = line(&lines, "Nested")?;
    let marker = line(&lines, "1.")?;
    assert!((marker.x - before.x).abs() < 0.2, "{lines:?}");
    assert!((first.x - before.x - 21.0).abs() < 0.2, "{lines:?}");
    assert!((first.y - marker.y).abs() < 0.2, "{lines:?}");
    assert!((before.y - first.y - 21.0).abs() < 0.2, "{lines:?}");
    assert!(
        (first.y - line(&lines, "Second")?.y - 21.0).abs() < 0.2,
        "{lines:?}"
    );
    // Nested bullet starts 27 pt inside the body, with a 9 pt marker box.
    assert!(
        (line(&lines, "•")?.x - before.x - 27.0).abs() < 0.2,
        "{lines:?}"
    );
    assert!((nested.x - before.x - 36.0).abs() < 0.2, "{lines:?}");
    Ok(())
}

#[test]
fn markdown_table_rows_match_writer_spacing() -> TestResult {
    let lines = pdf_lines(
        "Before\n\n| Header | Value |\n|---|---:|\n| First | 1 |\n| Second | 2 |\n\nAfter",
    )?;
    let before = line(&lines, "Before")?;
    let header = line(&lines, "Header")?;
    let first = line(&lines, "First")?;
    let second = line(&lines, "Second")?;
    assert!((header.x - before.x - 5.5).abs() < 0.2, "{lines:?}");
    for distance in [before.y - header.y, header.y - first.y, first.y - second.y] {
        assert!((distance - 21.25).abs() < 0.2, "{lines:?}");
    }
    assert!(header.font.contains("Bold"));
    Ok(())
}

#[test]
fn markdown_code_lines_match_writer_baseline_distance() -> TestResult {
    let lines = pdf_lines("```\nfirst = 1\nsecond = 2\n```")?;
    let distance = line(&lines, "first")?.y - line(&lines, "second")?.y;
    assert!((distance - 15.0).abs() < 0.2, "{lines:?}");
    Ok(())
}

#[test]
fn markdown_code_keeps_empty_lines_and_literal_markup() -> TestResult {
    let source = "```\n<img src=\"../outside.png\">\n\nlast & value\n```";
    let html = markdown::to_html(source);
    assert!(html.contains("&lt;img"), "{html}");
    assert!(!html.contains("<img"), "{html}");
    assert!(html.contains("&amp;"), "{html}");
    let lines = pdf_lines(source)?;
    let first = line(&lines, "<img")?;
    let last = line(&lines, "last & value")?;
    assert!((first.y - last.y - 30.0).abs() < 0.2, "{lines:?}");
    Ok(())
}

#[test]
fn markdown_inline_word_geometry() -> TestResult {
    let lines = pdf_lines(
        "Przychody **wzrosły o 12%**, a marża *pozostała stabilna*. Wynik jest ***bardzo dobry***.",
    )?;
    let start = line(&lines, "Przychody")?.x;
    // Relative starts in the same generated DOCX rendered by Writer.
    for (text, x) in [("wzrosły", 58.55), ("pozostała", 188.05), ("bardzo", 341.4)] {
        assert!((line(&lines, text)?.x - start - x).abs() < 0.5, "{lines:?}");
    }
    assert!(lines.iter().any(|l| l.font.contains("BoldItalic")));
    Ok(())
}
