//! Producer-to-consumer regressions for source identity and reader semantics.
use super::*;
use crate::{docxxml, markdown, pdfedit};

fn request(dir: &Scratch, value: Value) -> Result<Output> {
    Ok(run(&serde_json::from_value(value)?, &Options::new(&dir.0))?)
}
fn xml_text(node: &docxxml::Element) -> String {
    let mut text = node.text.clone();
    for child in &node.children {
        text.push_str(&xml_text(child));
    }
    text
}

#[test]
fn r16_pdf_paragraph_keeps_its_origin_after_a_table_prefix() -> Result {
    let dir = Scratch::new()?;
    request(
        &dir,
        json!({"op":"create", "output":"source.pdf", "markdown":"# Agreement\n\n| Owner | Duty |\n|---|---|\n| Consultant | Both parties |\n| Client | Review |\n\n## Confidentiality\n\nBoth parties will protect confidential information.\n"}),
    )?;
    request(
        &dir,
        json!({"op":"convert", "input":"source.pdf", "output":"out.docx"}),
    )?;
    let pdf = crate::pdfbudget::load(&std::fs::read(dir.0.join("source.pdf"))?)?;
    let page = pdf.get_pages().into_values().next().ok_or("page")?;
    let lines = pdfedit::positioned_lines(&pdf, page)?;
    let source = lines
        .iter()
        .find(|l| l.text.starts_with("Both parties will"))
        .ok_or("source paragraph")?;
    let earlier = lines
        .iter()
        .find(|l| l.text == "Both parties")
        .ok_or("table prefix")?;
    assert!((source.x - earlier.x).abs() > 100.0);
    let bytes = std::fs::read(dir.0.join("out.docx"))?;
    let root = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("XML")?)?;
    let paragraph = root
        .child("body")
        .ok_or("body")?
        .children
        .iter()
        .find(|p| p.name == "p" && xml_text(p).starts_with("Both parties will"))
        .ok_or("paragraph")?;
    let frame = paragraph
        .child("pPr")
        .and_then(|p| p.child("framePr"))
        .ok_or("frame")?;
    let x: f64 = frame.attrs.get("x").ok_or("x")?.parse()?;
    assert!(
        (x / 20.0 - source.x).abs() < 0.1,
        "frame {x}, source {source:?}"
    );
    Ok(())
}

#[test]
fn r16_vector_text_identity_does_not_match_another_occurrence() -> Result {
    let dir = Scratch::new()?;
    let mut pdf = crate::round13_tests::stream_pdf(b"q .2 .4 .6 rg 100 380 300 120 re f Q BT /F1 12 Tf 110 420 Td (Repeated label) Tj ET BT /F1 12 Tf 56 200 Td (Repeated label) Tj ET")?;
    let page = pdf.get_pages().into_values().next().ok_or("page")?;
    let font = pdf.add_object(
        lopdf::dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica"},
    );
    pdf.get_dictionary_mut(page)?.set(
        "Resources",
        lopdf::dictionary! {"Font"=>lopdf::dictionary! {"F1"=>font}},
    );
    pdf.save(dir.0.join("in.pdf"))?;
    let out = request(
        &dir,
        json!({"op":"convert", "input":"in.pdf", "output":"out.docx"}),
    )?;
    assert!(
        out.warnings
            .iter()
            .any(|w| w.code == "pdfdocx.vector_text_rasterized")
    );
    let bytes = std::fs::read(dir.0.join("out.docx"))?;
    let root = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("XML")?)?;
    assert_eq!(
        xml_text(&root).matches("Repeated label").count(),
        1,
        "only the occurrence outside the raster crop remains editable"
    );
    Ok(())
}

#[test]
fn r16_html_heading_and_literal_block_prefixes_roundtrip() -> Result {
    let dir = Scratch::new()?;
    for (index, literal) in [
        "1. Scope of services",
        "- Literal dash",
        "# Literal hash",
        "7) Literal number",
        "1. <em>Scope</em>",
    ]
    .iter()
    .enumerate()
    {
        for level in 1..=6 {
            let html = format!("<h{level}>{literal}</h{level}><p>{literal}</p>");
            save(&dir, "in.html", &html)?;
            request(
                &dir,
                json!({"op":"convert", "input":"in.html", "output":"out.md"}),
            )?;
            let md = std::fs::read_to_string(dir.0.join("out.md"))?;
            let out = read(&dir, "in.html", json!({}))?;
            assert_eq!(text(&out), md.trim(), "case {index}");
            let blocks = markdown::to_blocks(&md);
            assert!(
                matches!(blocks.first(), Some(markdown::Block::Heading(actual, _)) if *actual == level),
                "{md}"
            );
            assert_eq!(blocks.len(), 2, "{md}: {blocks:?}");
            assert!(matches!(blocks[1], markdown::Block::Paragraph(_)), "{md}");
            request(
                &dir,
                json!({"op":"convert", "input":"in.html", "output":"out.docx"}),
            )?;
            let out = read(&dir, "out.docx", json!({}))?;
            assert!(
                text(&out).starts_with(&format!("{} ", "#".repeat(level as usize))),
                "{}",
                text(&out)
            );
            assert!(!text(&out).contains("----"));
        }
    }
    Ok(())
}

#[test]
fn r16_docx_numbering_starts_overrides_counters_and_selection() -> Result {
    let dir = Scratch::new()?;
    let paragraph = |level: usize, text: &str| {
        format!(
            "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"8\"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"
        )
    };
    let document = format!(
        "<w:document xmlns:w=\"urn:w\"><w:body>{}{}{}{}{}</w:body></w:document>",
        paragraph(0, "First"),
        paragraph(1, "Child"),
        paragraph(1, "Child two"),
        paragraph(0, "Second"),
        paragraph(1, "Restarted child")
    );
    let numbering = r#"<w:numbering xmlns:w="urn:w"><w:abstractNum w:abstractNumId="4"><w:lvl w:ilvl="0"><w:start w:val="5"/><w:numFmt w:val="decimal"/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="2"/><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum><w:num w:numId="8"><w:abstractNumId w:val="4"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="7"/></w:lvlOverride><w:lvlOverride w:ilvl="1"><w:lvl w:ilvl="1"><w:start w:val="3"/><w:numFmt w:val="decimal"/></w:lvl></w:lvlOverride></w:num></w:numbering>"#;
    save(
        &dir,
        "in.docx",
        zip(&[
            ("word/document.xml", &document),
            ("word/numbering.xml", numbering),
        ]),
    )?;
    let out = read(&dir, "in.docx", json!({}))?;
    for expected in [
        "7. First",
        "    3. Child",
        "    4. Child two",
        "8. Second",
        "    3. Restarted child",
    ] {
        assert!(text(&out).contains(expected), "{expected}: {}", text(&out));
    }
    let selected = read(&dir, "in.docx", json!({"range":"4"}))?;
    assert_eq!(text(&selected), "8. Second");
    // Markdown semantics: the consumer must see 7, not merely the item words.
    assert!(matches!(
        markdown::to_blocks(text(&out))[0],
        markdown::Block::ListItem { number: 7, .. }
    ));
    Ok(())
}

#[test]
fn r16_odt_inherited_emphasis_and_list_starts_are_read() -> Result {
    let dir = Scratch::new()?;
    let styles = r#"<office:document-styles xmlns:office="urn:office" xmlns:style="urn:style" xmlns:fo="urn:fo" xmlns:text="urn:text"><office:styles><style:style style:name="Bold"><style:text-properties fo:font-weight="bold"/></style:style><style:style style:name="Child" style:parent-style-name="Bold"/><style:style style:name="Italic"><style:text-properties fo:font-style="italic"/></style:style><text:list-style style:name="Ordered"><text:list-level-style-number text:level="1" text:start-value="7"/></text:list-style></office:styles></office:document-styles>"#;
    let content = r#"<office:document-content xmlns:office="urn:office" xmlns:text="urn:text" xmlns:style="urn:style" xmlns:fo="urn:fo"><office:automatic-styles><style:style style:name="Plain" style:parent-style-name="Bold"><style:text-properties fo:font-weight="normal"/></style:style></office:automatic-styles><office:body><office:text><text:p text:style-name="Child">Company</text:p><text:p>Fee <text:span text:style-name="Bold">1500 USD</text:span> and <text:span text:style-name="Italic">signed</text:span>.</text:p><text:p text:style-name="Plain">Normal</text:p><text:list text:style-name="Ordered"><text:list-item><text:p>First</text:p></text:list-item><text:list-item><text:p>Second</text:p></text:list-item><text:list-item text:start-value="12"><text:p>Override</text:p></text:list-item></text:list></office:text></office:body></office:document-content>"#;
    save(
        &dir,
        "in.odt",
        zip(&[("content.xml", content), ("styles.xml", styles)]),
    )?;
    let out = read(&dir, "in.odt", json!({}))?;
    for expected in [
        "**Company**",
        "**1500 USD**",
        "*signed*",
        "7. First",
        "8. Second",
        "12. Override",
    ] {
        assert!(text(&out).contains(expected), "{expected}: {}", text(&out));
    }
    assert!(!text(&out).contains("**Normal**"));
    Ok(())
}

#[test]
fn r16_backend_warnings_use_original_page_numbers() -> Result {
    let dir = Scratch::new()?;
    let mut pdf =
        crate::round13_tests::stream_pdf(b"BT /F1 12 Tf 56 650 Td (Page text) Tj ET /Sh1 sh")?;
    let page = pdf.get_pages().into_values().next().ok_or("page")?;
    let font = pdf.add_object(
        lopdf::dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica"},
    );
    pdf.get_dictionary_mut(page)?.set(
        "Resources",
        lopdf::dictionary! {"Font"=>lopdf::dictionary! {"F1"=>font}},
    );
    pdf.save(dir.0.join("in.pdf"))?;
    request(
        &dir,
        json!({"op":"merge", "inputs":["in.pdf","in.pdf","in.pdf"], "output":"three.pdf"}),
    )?;
    let out = read(&dir, "three.pdf", json!({"pages":"3"}))?;
    let warnings: Vec<_> = out
        .warnings
        .iter()
        .filter(|w| w.code == "pdfdocx.bounds_approximate")
        .collect();
    assert!(!warnings.is_empty(), "{:?}", out.warnings);
    for warning in warnings {
        assert_eq!(warning.params["page"], 3);
    }
    Ok(())
}

#[test]
fn r16_diagnostic_preview_keeps_the_original_count_idempotently() -> Result {
    let dir = Scratch::new()?;
    save(&dir, "in.txt", "No match")?;
    let out = request(
        &dir,
        json!({"op":"replace", "input":"in.txt", "output":"out.txt", "find":"x".repeat(6000), "replace":"y"}),
    )?;
    assert_eq!(out.summary_params["truncated_fields"]["find"], 6002);
    assert_eq!(
        crate::messages::bounded_params(out.summary_params.clone()),
        out.summary_params
    );
    assert!(serde_json::to_vec(&out)?.len() < 2000);
    Ok(())
}

#[test]
fn r16_pdf_producers_preserve_link_destinations() -> Result {
    let dir = Scratch::new()?;
    let markdown = "[Reference](https://example.org/a) and [Reference](https://example.org/b).";
    save(&dir, "in.md", markdown)?;
    save(
        &dir,
        "in.html",
        "<p><a href=\"https://example.org/a\">Reference</a> and <a href=\"https://example.org/b\">Reference</a>.</p>",
    )?;
    request(
        &dir,
        json!({"op":"create", "markdown":markdown, "output":"in.docx"}),
    )?;
    let word = read(&dir, "in.docx", json!({}))?;
    assert!(text(&word).contains("https://example.org/a"));
    assert!(text(&word).contains("https://example.org/b"));
    for source in ["in.md", "in.html", "in.docx"] {
        let out = request(
            &dir,
            json!({"op":"convert", "input":source, "output":"out.pdf"}),
        )?;
        assert!(!out.warnings.iter().any(|w| w.code == "pdf.links_omitted"));
        let native = read(&dir, "out.pdf", json!({}))?;
        assert!(text(&native).contains("https://example.org/a"));
        assert!(text(&native).contains("https://example.org/b"));
        let consumer = std::process::Command::new("pdfinfo")
            .args(["-url", "out.pdf"])
            .current_dir(&dir.0)
            .output()?;
        assert!(consumer.status.success(), "{consumer:?}");
        let urls = String::from_utf8(consumer.stdout)?;
        assert!(urls.contains("https://example.org/a") && urls.contains("https://example.org/b"));
    }
    let out = request(
        &dir,
        json!({"op":"create", "markdown":markdown, "output":"created.pdf"}),
    )?;
    assert!(!out.warnings.iter().any(|w| w.code == "pdf.links_omitted"));
    let native = read(&dir, "created.pdf", json!({}))?;
    assert!(text(&native).contains("https://example.org/a"));
    assert!(text(&native).contains("https://example.org/b"));
    Ok(())
}

#[test]
fn r16_html_image_keeps_author_dimensions_in_pdf() -> Result {
    let dir = Scratch::new()?;
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        960,
        334,
        image::Rgb([20, 90, 160]),
    ))
    .save(dir.0.join("chart.png"))?;
    for attributes in [
        "width=\"540\" height=\"188\"",
        "style=\"width:405pt;height:141pt\"",
        "width=\"540\"",
    ] {
        save(
            &dir,
            "in.html",
            format!("<html><body><img src=\"chart.png\" {attributes}></body></html>"),
        )?;
        request(
            &dir,
            json!({"op":"convert", "input":"in.html", "output":"out.pdf"}),
        )?;
        let pdf = crate::pdfbudget::load(&std::fs::read(dir.0.join("out.pdf"))?)?;
        let page = pdf.get_pages().into_values().next().ok_or("page")?;
        let bounds = pdfedit::page_bounds(&pdf, page)?;
        let ink = bounds.ink.ok_or("image bounds")?.0;
        assert!(
            ((ink[2] - ink[0]) - 405.0).abs() < 1.0,
            "{attributes}: {ink:?}"
        );
        assert!(
            ((ink[3] - ink[1]) - 141.0).abs() < 1.0,
            "{attributes}: {ink:?}"
        );
    }
    Ok(())
}
