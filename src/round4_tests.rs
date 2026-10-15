//! Generated regression documents for the fourteen conversion requirements.
use super::*;
use docx_rs::{Docx, Paragraph, Run, RunFonts, Style, StyleType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn xml(bytes: &[u8], name: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(String::from_utf8(
        docxxml::entry(bytes, name)?.ok_or("missing entry")?,
    )?)
}

#[test]
fn r4_ooxml_rejects_corrupt_crc() -> TestResult {
    let mut bytes = docx::pack(Docx::new())?;
    let mut changed = false;
    for at in 0..bytes.len().saturating_sub(64) {
        if bytes.get(at..at + 4) == Some(b"PK\x01\x02")
            && bytes.get(at + 46..at + 63) == Some(b"word/document.xml")
        {
            bytes[at + 16] ^= 1;
            changed = true;
            break;
        }
    }
    assert!(changed);
    assert!(docxxml::entry(&bytes, "word/document.xml").is_err());
    Ok(())
}

#[test]
fn r4_ooxml_rejects_deep_xml() -> TestResult {
    let xml = format!("{}{}", "<x>".repeat(130), "</x>".repeat(130));
    assert!(docxxml::parse(xml.as_bytes()).is_err());
    assert!(docxxml::parse(b"<x><y/></x>").is_ok());
    Ok(())
}

fn render_docx(
    doc: Docx,
) -> Result<(docxhtml::DocxHtml, lopdf::Document), Box<dyn std::error::Error>> {
    let bytes = docx::pack(doc)?;
    let parsed = docx::parse(&bytes)?;
    let converted = docxhtml::convert(&bytes, &parsed)?;
    let images = converted
        .images
        .iter()
        .map(|(k, v)| (k.clone(), printpdf::Base64OrRaw::Raw(v.clone())))
        .collect();
    let pdf = pdf::html_to_pdf_with_families(
        &markdown::full_html(&converted.body),
        &pdf::load_font(None)?,
        &images,
        &converted.setup,
        &converted.families,
        &mut Vec::new(),
    )?;
    Ok((converted, lopdf::Document::load_mem(&pdf)?))
}

fn lines(doc: &lopdf::Document) -> Result<Vec<pdfedit::PositionedLine>, Error> {
    doc.get_pages()
        .into_values()
        .map(|p| pdfedit::positioned_lines(doc, p))
        .collect::<Result<Vec<_>, _>>()
        .map(|v| v.into_iter().flatten().collect())
}
fn markdown_doc(markdown: &str) -> Result<Vec<u8>, Error> {
    docx::blocks_to_docx(&markdown::to_blocks(markdown))
}
fn markdown_pdf(markdown: &str) -> Result<lopdf::Document, Error> {
    Ok(lopdf::Document::load_mem(&pdf::html_to_pdf(
        &markdown::full_html(&markdown::to_html(markdown)),
        &pdf::load_font(None)?,
    )?)?)
}

#[test]
fn r4_01_heading_inherits_styles_and_defaults() -> TestResult {
    let (html, pdf) = render_docx(
        Docx::new()
            .default_size(22)
            .add_style(
                Style::new("BaseHeading", StyleType::Paragraph)
                    .bold()
                    .size(40),
            )
            .add_style(Style::new("Title", StyleType::Paragraph).based_on("BaseHeading"))
            .add_paragraph(
                Paragraph::new()
                    .style("Title")
                    .add_run(Run::new().add_text("Styled title")),
            )
            .add_paragraph(Paragraph::new().add_run(Run::new().add_text("Body text"))),
    )?;
    assert!(html.body.contains("font-weight:bold"));
    let rows = lines(&pdf)?;
    assert!(
        rows.iter()
            .any(|r| r.text.contains("Styled title") && r.size > 19.0 && r.font.contains("Bold")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.text.contains("Body text") && (r.size - 11.0).abs() < 0.1)
    );
    Ok(())
}

#[test]
fn r4_02_run_properties_survive() -> TestResult {
    let (html, pdf) = render_docx(
        Docx::new().add_paragraph(
            Paragraph::new()
                .add_run(Run::new().add_text("Before "))
                .add_run(
                    Run::new()
                        .add_text("Kowalski Sp. z o.o.")
                        .bold()
                        .italic()
                        .underline("single")
                        .color("123456")
                        .size(28)
                        .fonts(RunFonts::new().ascii("Noto Sans")),
                )
                .add_run(Run::new().add_text(" after")),
        ),
    )?;
    for css in [
        "font-weight:bold",
        "font-style:italic",
        "text-decoration:underline",
        "color:#123456",
        "font-size:14pt",
        "Noto Sans",
    ] {
        assert!(html.body.contains(css), "{css}: {}", html.body);
    }
    assert!(lines(&pdf)?.iter().any(|r| r.text.contains("Kowalski")
        && r.font.contains("Bold")
        && r.font.contains("Italic")));
    Ok(())
}

#[test]
fn r4_03_table_geometry_and_merges_from_xml() -> TestResult {
    use docx_rs::{Table, TableCell, TableRow, VMergeType};
    let cell = |text: &str| {
        TableCell::new().add_paragraph(Paragraph::new().add_run(Run::new().add_text(text)))
    };
    let (html, pdf) = render_docx(
        Docx::new().add_table(
            Table::new(vec![
                TableRow::new(vec![cell("Merged header").grid_span(2)]),
                TableRow::new(vec![
                    cell("Vertical").vertical_merge(VMergeType::Restart),
                    cell("Right one"),
                ]),
                TableRow::new(vec![
                    cell("").vertical_merge(VMergeType::Continue),
                    cell("Right two"),
                ]),
            ])
            .set_grid(vec![1800, 2400]),
        ),
    )?;
    assert!(html.body.contains("width:210pt"));
    assert!(html.body.contains("colspan=\"2\""));
    assert!(html.body.contains("rowspan=\"2\""));
    assert!(
        !html
            .warnings
            .iter()
            .any(|v| v.code == MessageCode::DocxTableMerges)
    );
    let text = pdfedit::document_text(&pdf);
    assert_eq!(text.matches("Merged header").count(), 1, "{text}");
    assert_eq!(text.matches("Vertical").count(), 1, "{text}");
    Ok(())
}

#[test]
fn r4_04_document_font_and_named_substitution() -> TestResult {
    let mut warnings = Vec::new();
    let bytes = pdf::html_to_pdf_with_families(
        &markdown::full_html(
            "<p style=\"font-family:'Unavailable Serif Example'\">Missing font</p><p style=\"font-family:'Noto Sans'\">Present font</p>",
        ),
        &pdf::load_font(None)?,
        &Default::default(),
        &Default::default(),
        &["Unavailable Serif Example".into(), "Noto Sans".into()],
        &mut warnings,
    )?;
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].params.get("font"),
        Some(&serde_json::json!("Unavailable Serif Example"))
    );
    assert_eq!(
        warnings[0].params.get("substitute"),
        Some(&serde_json::json!("Noto Serif"))
    );
    let rows = lines(&lopdf::Document::load_mem(&bytes)?)?;
    assert!(
        rows.iter().any(|r| r.font.contains("NotoSerif")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|r| r.font.contains("NotoSans")), "{rows:?}");
    Ok(())
}

#[test]
fn r4_05_markdown_table_header_is_first_row() -> TestResult {
    let bytes = markdown_doc(
        "| Region | Q2 | Q3 | Zmiana |\n|---|---:|---:|---:|\n| North | 1 | 2 | 100% |",
    )?;
    let document = xml(&bytes, "word/document.xml")?;
    let head = document.find("Region").ok_or("missing header")?;
    assert!(head < document.find("North").ok_or("missing row")?);
    assert!(document.contains("tblHeader"));
    Ok(())
}

const LIST: &str = "1. Revenue increased.\n2. Koszty marketingu spadły o 7%.\n   - Nested first\n   - Nested second\n3. Final item.\n";
#[test]
fn r4_06_real_nested_word_lists() -> TestResult {
    let bytes = markdown_doc(LIST)?;
    crate::round11_tests::numbering(&bytes)?;
    let document = xml(&bytes, "word/document.xml")?;
    for text in [
        "Revenue increased.",
        "Koszty marketingu spadły o 7%.",
        "Nested first",
        "Nested second",
        "Final item.",
    ] {
        assert_eq!(document.matches(text).count(), 1);
    }
    assert!(document.contains("w:ilvl w:val=\"1\""));
    let numbering = xml(&bytes, "word/numbering.xml")?;
    assert!(numbering.contains("decimal"));
    assert!(numbering.contains("bullet"));
    Ok(())
}
#[test]
fn r4_07_readable_markdown_document_defaults() -> TestResult {
    let bytes = markdown_doc("# One\n\n## Two\n\n### Three\n\nBody")?;
    let styles = xml(&bytes, "word/styles.xml")?;
    assert!(styles.contains("Noto Sans"));
    assert!(styles.contains("w:sz w:val=\"22\""));
    assert!(styles.contains("w:after=\"120\""));
    let document = xml(&bytes, "word/document.xml")?;
    for size in [40, 32, 26] {
        assert!(document.contains(&format!("w:sz w:val=\"{size}\"")));
    }
    Ok(())
}
#[test]
fn r4_08_quote_code_link_and_right_alignment() -> TestResult {
    let bytes = markdown_doc(
        "> A quotation\n\n```\nlet answer = 42;\n```\n\n[Reference](https://example.com)\n\n| Label | Value |\n|---|---:|\n| Sample | 42 |",
    )?;
    let document = xml(&bytes, "word/document.xml")?;
    for token in [
        "Quote",
        "Noto Sans Mono",
        "F2F2F2",
        "hyperlink",
        "w:val=\"right\"",
    ] {
        assert!(document.contains(token), "{token}: {document}");
    }
    let styles = xml(&bytes, "word/styles.xml")?;
    assert!(styles.contains("w:i"));
    assert!(styles.contains("w:left=\"360\""));
    assert!(xml(&bytes, "word/_rels/document.xml.rels")?.contains("https://example.com"));
    Ok(())
}
#[test]
fn r4_09_pdf_inline_uses_four_faces() -> TestResult {
    let pdf = markdown_pdf("Normal **Bold** *Italic* ***Both*** end.")?;
    let rows = lines(&pdf)?;
    for (text, bold, italic) in [
        ("Normal", false, false),
        ("Bold", true, false),
        ("Italic", false, true),
        ("Both", true, true),
    ] {
        assert!(
            rows.iter().any(|r| r.text.contains(text)
                && r.font.contains("Bold") == bold
                && r.font.contains("Italic") == italic),
            "{text}: {rows:?}"
        );
    }
    Ok(())
}
#[test]
fn r4_font_alias_is_not_replaced_in_visible_text() -> TestResult {
    let text = pdfedit::document_text(&markdown_pdf("The literal name is DokuFont.")?);
    assert!(text.contains("The literal name is DokuFont."), "{text}");
    Ok(())
}

#[test]
fn r4_10_nested_pdf_list_item_occurs_once() -> TestResult {
    let text = pdfedit::document_text(&markdown_pdf(LIST)?);
    for sentence in [
        "Koszty marketingu spadły o 7%.",
        "Nested first",
        "Nested second",
        "Final item.",
    ] {
        assert_eq!(text.matches(sentence).count(), 1, "{text}");
    }
    Ok(())
}
#[test]
fn r4_11_pdf_heading_header_quote_and_code() -> TestResult {
    let pdf = markdown_pdf(
        "# Heading\n\n| Header | Value |\n|---|---:|\n| Data | 42 |\n\n> Quote\n\n```\nmonospace code\n```\n",
    )?;
    let rows = lines(&pdf)?;
    assert!(
        rows.iter()
            .any(|r| r.text.contains("Heading") && r.font.contains("Bold") && r.size > 19.0),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.text.contains("Header") && r.font.contains("Bold")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.text.contains("Quote") && r.font.contains("Italic")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.text.contains("monospace code")
            && (r.font.contains("Mono") || r.font.contains("Courier"))),
        "{rows:?}"
    );
    Ok(())
}

fn contract_pdf() -> Result<lopdf::Document, Box<dyn std::error::Error>> {
    use lopdf::{Object, Stream, dictionary};
    let mut doc = lopdf::Document::with_version("1.5");
    let regular = doc.add_object(dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica", "Encoding"=>"WinAnsiEncoding"});
    let bold = doc.add_object(dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica-Bold", "Encoding"=>"WinAnsiEncoding"});
    let mut content = String::from(
        "BT /F1 18 Tf 1 0 0 1 50 750 Tm (Contract title) Tj ET\nBT /F1 12 Tf 1 0 0 1 50 720 Tm (Between ) Tj /F2 12 Tf (Kowalski Sp.) Tj /F1 12 Tf ( and client.) Tj ET\n",
    );
    for y in [690, 670, 650] {
        content.push_str(&format!("50 {y} m 350 {y} l S\n"));
    }
    for x in [50, 200, 350] {
        content.push_str(&format!("{x} 650 m {x} 690 l S\n"));
    }
    content.push_str("30 610 m 100 610 l S\n400 610 m 550 610 l S\n");
    for (x, y, text) in [
        (60, 675, "Service"),
        (210, 675, "Price"),
        (60, 655, "Support"),
        (210, 655, "1500"),
    ] {
        content.push_str(&format!("BT /F1 12 Tf 1 0 0 1 {x} {y} Tm ({text}) Tj ET\n"));
    }
    content.push_str("BT /F1 10 Tf 1 0 0 1 50 80 Tm (Footer) Tj ET\n");
    let stream = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let pages = doc.new_object_id();
    let page = doc.add_object(dictionary!{"Type"=>"Page", "Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Contents"=>stream,"Resources"=>dictionary!{"Font"=>dictionary!{"F1"=>regular,"F2"=>bold}}});
    doc.objects.insert(
        pages,
        Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>vec![page.into()],"Count"=>1}),
    );
    let root = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
    doc.trailer.set("Root", root);
    Ok(doc)
}
fn contract_docx() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let pdf = contract_pdf()?;
    Ok(pdfdocx::build(&pdf, &pdf.get_pages().into_values().collect::<Vec<_>>())?.data)
}
#[test]
fn r4_12_pdf_mixed_line_remains_one_paragraph_before_table() -> TestResult {
    let bytes = contract_docx()?;
    let root = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("no document")?)?;
    let body = root.child("body").ok_or("no body")?;
    let paragraph = body
        .children
        .iter()
        .position(|p| format!("{p:?}").contains("Between"))
        .ok_or("no paragraph")?;
    let p = format!("{:?}", body.children[paragraph]);
    assert!(p.contains("Kowalski Sp."));
    assert!(p.contains("and client."));
    assert!(p.contains("name: \"b\""));
    assert!(
        paragraph
            < body
                .children
                .iter()
                .position(|v| v.name == "tbl")
                .ok_or("no table")?
    );
    Ok(())
}
#[test]
fn r4_13_pdf_table_grid_ignores_signature_lines() -> TestResult {
    let bytes = contract_docx()?;
    let root = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("no document")?)?;
    let table = root.path(&["body", "tbl"]).ok_or("no table")?;
    assert_eq!(table.child("tblGrid").ok_or("no grid")?.children.len(), 2);
    for row in table.children.iter().filter(|v| v.name == "tr") {
        assert_eq!(row.children.iter().filter(|v| v.name == "tc").count(), 2);
    }
    Ok(())
}
#[test]
fn r4_14_pdf_font_sizes_page_and_margins() -> TestResult {
    let bytes = contract_docx()?;
    let root = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("no document")?)?;
    let size = root
        .path(&["body", "sectPr", "pgSz"])
        .ok_or("no page size")?;
    assert_eq!(size.attr("w"), Some("12000"));
    assert_eq!(size.attr("h"), Some("16000"));
    assert_eq!(
        root.path(&["body", "sectPr", "pgMar"])
            .ok_or("no margins")?
            .attr("left"),
        // The signature rule starts at 30 pt with a butt cap, so its
        // painted left bound is 30 pt, not the text origin at 50 pt.
        Some("600")
    );
    let document = xml(&bytes, "word/document.xml")?;
    assert!(document.contains("w:sz w:val=\"36\""));
    assert!(document.contains("w:sz w:val=\"24\""));
    Ok(())
}
