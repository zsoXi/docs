//! Generated fixtures exercise the public JSON contract, not private parsing helpers.

#[path = "round10_tests.rs"]
mod round10;
#[path = "round16_tests.rs"]
mod round16;

use super::raster::{Scratch, decode};
use crate::{Input, Options, Output, run};
use serde_json::{Value, json};
use std::error::Error as StdError;

type Result<T = ()> = std::result::Result<T, Box<dyn StdError>>;

fn read(dir: &Scratch, name: &str, extra: Value) -> Result<Output> {
    read_with(dir, name, extra, &Options::new(&dir.0))
}
fn read_with(_dir: &Scratch, name: &str, extra: Value, opts: &Options) -> Result<Output> {
    let mut value = json!({"op":"read","input":name,"locators":true});
    for (key, val) in extra.as_object().ok_or("request fields")? {
        value[key] = val.clone();
    }
    let input: Input = serde_json::from_value(value)?;
    let out = run(&input, opts)?;
    if let Some(data) = &out.data {
        let joined: String = data["chunks"]
            .as_array()
            .ok_or("chunks")?
            .iter()
            .map(|c| c["text"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            joined,
            text(&out),
            "chunk text must exactly reconstruct text"
        );
        for chunk in data["chunks"].as_array().ok_or("chunks")? {
            assert!(chunk["locator"].is_object());
        }
    }
    for warning in &out.warnings {
        assert!(
            !warning.message.contains('{'),
            "missing parameters: {warning:?}"
        );
    }
    Ok(out)
}
fn text(out: &Output) -> &str {
    out.text.as_deref().unwrap_or("")
}
fn chunks(out: &Output) -> Result<&Vec<Value>> {
    out.data
        .as_ref()
        .and_then(|d| d["chunks"].as_array())
        .ok_or_else(|| "chunks".into())
}
fn save(dir: &Scratch, name: &str, bytes: impl AsRef<[u8]>) -> Result {
    std::fs::write(dir.0.join(name), bytes)?;
    Ok(())
}

// Small stored ZIPs keep fixtures readable and avoid adding a ZIP test dependency.
fn zip(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut data = Vec::new();
    let mut central = Vec::new();
    for (name, content) in parts {
        let offset = data.len() as u32;
        let bytes = content.as_bytes();
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        crc = !crc;
        data.extend_from_slice(b"PK\x03\x04");
        for n in [20u16, 0, 0, 0, 0] {
            data.extend_from_slice(&n.to_le_bytes());
        }
        for n in [crc, bytes.len() as u32, bytes.len() as u32] {
            data.extend_from_slice(&n.to_le_bytes());
        }
        data.extend_from_slice(&(name.len() as u16).to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(name.as_bytes());
        data.extend_from_slice(bytes);
        central.extend_from_slice(b"PK\x01\x02");
        for n in [20u16, 20, 0, 0, 0, 0] {
            central.extend_from_slice(&n.to_le_bytes());
        }
        for n in [crc, bytes.len() as u32, bytes.len() as u32] {
            central.extend_from_slice(&n.to_le_bytes());
        }
        for n in [name.len() as u16, 0, 0, 0, 0] {
            central.extend_from_slice(&n.to_le_bytes());
        }
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let offset = data.len() as u32;
    let size = central.len() as u32;
    data.extend(central);
    data.extend_from_slice(b"PK\x05\x06");
    for n in [0u16, 0, parts.len() as u16, parts.len() as u16] {
        data.extend_from_slice(&n.to_le_bytes());
    }
    data.extend_from_slice(&size.to_le_bytes());
    data.extend_from_slice(&offset.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data
}

const HTML: &str = r##"<!doctype html><html><head><style>.x{color:red}</style><script>hiddenScript()</script></head><body><h1>Report</h1><p>Before <b>bold</b> after <a href="https://example.org/ref">reference</a>.</p><ul><li>First item<ul><li>Nested item</li></ul></li></ul><table><tr><th>Name</th><th>Value</th></tr><tr><td>Alpha</td><td>42</td></tr></table><figure><img src="missing.png" alt="Chart description"><figcaption>Figure caption</figcaption></figure><p>Footnote <a href="#note1">1</a></p><p id="note1">Note text.</p></body></html>"##;

#[test]
fn html_txt_md_keep_structure_and_unicode_limits() -> Result {
    let dir = Scratch::new()?;
    save(&dir, "in.html", HTML)?;
    let out = read(&dir, "in.html", json!({}))?;
    let value = text(&out);
    for expected in [
        "Report",
        "**bold**",
        "after",
        "[reference](https://example.org/ref)",
        "Nested item",
        "| Name",
        "Alpha",
        "Chart description",
        "Figure caption",
        "Note text",
    ] {
        assert!(value.contains(expected), "missing {expected}: {value}");
    }
    assert!(!value.contains("hiddenScript"));
    assert!(!value.contains("color:red"));
    for name in ["in.md", "in.txt"] {
        let source = "# Heading\n\nZażółć gęślą jaźń.\n\n- One\n- Two\n";
        save(&dir, name, source)?;
        assert_eq!(text(&read(&dir, name, json!({"locators":false}))?), source);
        let limited = read(&dir, name, json!({"max_chars":16}))?;
        assert_eq!(text(&limited).chars().count(), 17);
        assert_eq!(limited.warnings[0].code, "read.truncated");
        assert_eq!(
            limited.warnings[0].params["total_chars"],
            source.chars().count()
        );
        let selected = read(&dir, name, json!({"range":"2"}))?;
        assert_eq!(text(&selected), "Zażółć gęślą jaźń.");
        assert_eq!(chunks(&selected)?[0]["locator"]["block"], 2);
        let zero = read(&dir, name, json!({"max_chars":0}))?;
        assert_eq!(text(&zero), "");
    }
    Ok(())
}

#[test]
fn html_semantic_footnotes_keep_references_and_definitions() -> Result {
    let dir = Scratch::new()?;
    save(
        &dir,
        "notes.html",
        r##"<p>Claim<a role="doc-noteref" href="#note-1">1</a> continues.</p><aside id="note-1" role="doc-footnote"><p>Footnote evidence.</p></aside>"##,
    )?;
    let out = read(&dir, "notes.html", json!({}))?;
    assert!(text(&out).contains("Claim[^note-1] continues."));
    assert!(text(&out).contains("[^note-1]: Footnote evidence."));
    Ok(())
}

#[test]
fn docx_headings_lists_tables_links_notes_captions_and_paragraphs() -> Result {
    let dir = Scratch::new()?;
    let document = r#"<w:document xmlns:w="urn:w" xmlns:r="urn:r" xmlns:wp="urn:wp"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Report</w:t></w:r></w:p>
<w:p><w:r><w:t>Before </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:t> after </w:t></w:r><w:hyperlink r:id="link"><w:r><w:t>reference</w:t></w:r></w:hyperlink><w:r><w:footnoteReference w:id="1"/></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>List item</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>Alpha</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:drawing><wp:docPr descr="Chart caption"/></w:drawing></w:r></w:p>
</w:body></w:document>"#;
    let bytes = zip(&[
        ("word/document.xml", document),
        (
            "word/_rels/document.xml.rels",
            r#"<Relationships><Relationship Id="link" Target="https://example.org/ref" TargetMode="External" Type="hyperlink"/></Relationships>"#,
        ),
        (
            "word/footnotes.xml",
            r#"<w:footnotes xmlns:w="urn:w"><w:footnote w:id="1"><w:p><w:r><w:t>Footnote body</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
        ),
    ]);
    save(&dir, "in.docx", bytes)?;
    let out = read(&dir, "in.docx", json!({}))?;
    for expected in [
        "# Report",
        "Before **bold** after",
        "[reference](https://example.org/ref)",
        "- List item",
        "| Name | Value |",
        "| Alpha | 42 |",
        "[^fn1]",
        "[^fn1]: Footnote body",
        "Chart caption",
    ] {
        assert!(text(&out).contains(expected), "{expected}: {}", text(&out));
    }
    assert_eq!(chunks(&out)?[1]["locator"]["paragraph"], 2);
    assert_eq!(chunks(&out)?[4]["locator"]["paragraph"], 8);
    let one = read(&dir, "in.docx", json!({"range":"2"}))?;
    assert!(!text(&one).contains("Report"));
    assert!(text(&one).contains("Footnote body"));
    Ok(())
}

const NOTE: &str = "# Report\n\nBefore **bold** after.\n\n- First item\n- Second item\n\n| Name | Value |\n|---|---|\n| Alpha | 42 |\n\n[reference](https://example.org/ref)\n";

fn office_convert(dir: &Scratch, input: &str, format: &str) -> Result {
    decode(
        dir,
        "soffice",
        &[
            "-env:UserInstallation=file:///tmp/profile",
            "--headless",
            "--nologo",
            "--convert-to",
            format,
            "--outdir",
            "/work",
            &format!("/work/{input}"),
        ],
    )?;
    Ok(())
}

#[test]
fn libreoffice_odt_rtf_doc_read_twice_and_concurrently() -> Result {
    let dir = Scratch::new()?;
    run(
        &Input::Create {
            markdown: NOTE.into(),
            output: "source.docx".into(),
        },
        &Options::new(&dir.0),
    )?;
    for format in ["odt", "rtf", "doc:MS Word 97"] {
        office_convert(&dir, "source.docx", format)?;
    }
    for name in ["source.odt", "source.rtf", "source.doc"] {
        let first = read(&dir, name, json!({}))?;
        let second = read(&dir, name, json!({}))?;
        assert_eq!(text(&first), text(&second));
        for expected in [
            "Report",
            "First item",
            "Second item",
            "| Name | Value |",
            "| Alpha | 42 |",
            "https://example.org/ref",
        ] {
            assert!(
                text(&first).contains(expected),
                "{name}: {expected}: {}",
                text(&first)
            );
        }
        assert!(text(&first).contains("# "), "heading in {name}");
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    read(&dir, "source.doc", json!({}))
                        .map(|v| text(&v).to_string())
                        .map_err(|e| e.to_string())
                })
            })
            .collect();
        for handle in handles {
            assert!(
                handle.join().is_ok_and(|r| r.is_ok()),
                "concurrent legacy read"
            );
        }
    });
    Ok(())
}

#[test]
fn epub_spine_not_manifest_order_with_navigation() -> Result {
    let dir = Scratch::new()?;
    let package = r#"<package><manifest><item id="last" href="last.xhtml"/><item id="nav" href="nav.xhtml" properties="nav"/><item id="first" href="first.xhtml"/></manifest><spine><itemref idref="first"/><itemref idref="last"/></spine></package>"#;
    save(
        &dir,
        "book.epub",
        zip(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#,
            ),
            ("OPS/book.opf", package),
            (
                "OPS/first.xhtml",
                "<h1>First chapter</h1><ul><li>Opening item</li></ul>",
            ),
            (
                "OPS/last.xhtml",
                "<h1>Last chapter</h1><p>Closing paragraph.</p>",
            ),
            (
                "OPS/nav.xhtml",
                r#"<nav><ol><li><a href="first.xhtml">Start here</a></li><li><a href="last.xhtml">Finish here</a></li></ol></nav>"#,
            ),
        ]),
    )?;
    let out = read(&dir, "book.epub", json!({}))?;
    assert!(text(&out).contains("# Contents"));
    assert!(text(&out).contains("[Start here](first.xhtml)"));
    assert!(text(&out).find("First chapter") < text(&out).find("Last chapter"));
    assert_eq!(chunks(&out)?[1]["locator"]["chapter"], 1);
    assert_eq!(chunks(&out)?[1]["locator"]["href"], "OPS/first.xhtml");
    let second = read(&dir, "book.epub", json!({"range":"2"}))?;
    assert!(!text(&second).contains("Opening item"));
    assert!(text(&second).contains("Closing paragraph"));
    Ok(())
}

#[test]
fn mhtml_decodes_folded_headers_base64_and_quoted_printable() -> Result {
    let dir = Scratch::new()?;
    let source = "MIME-Version: 1.0\r\nContent-Type: multipart/related;\r\n boundary=\"part\"\r\n\r\n--part\r\nContent-Type: text/html; charset=utf-8\r\nContent-Location: https://example.org/page\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<h1>Archive</h1><p>Quoted =3D value =\r\ncontinued.</p><ul><li>Archive item</li></ul>\r\n--part\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nPGgxPkZyYW1lPC9oMT4=\r\n--part--\r\n";
    save(&dir, "in.mhtml", source)?;
    let out = read(&dir, "in.mhtml", json!({}))?;
    for expected in [
        "Archive",
        "Quoted = value continued.",
        "Archive item",
        "Frame",
    ] {
        assert!(text(&out).contains(expected), "{expected}: {}", text(&out));
    }
    assert_eq!(
        chunks(&out)?[0]["locator"]["location"],
        "https://example.org/page"
    );
    assert_eq!(chunks(&out)?.len(), 2);
    Ok(())
}

#[test]
fn xmind_json_and_legacy_xml_nested_topics_and_notes() -> Result {
    let dir = Scratch::new()?;
    let json = r#"[{"title":"Plan","rootTopic":{"title":"Root","notes":{"plain":{"content":"Root note"}},"children":{"attached":[{"title":"Child","children":{"attached":[{"title":"Grandchild"}]}}]}}}]"#;
    let xml = "<xmap-content><sheet><title>Plan</title><topic><title>Root</title><notes><plain>Root note</plain></notes><children><topics type=\"attached\"><topic><title>Child</title><children><topics><topic><title>Grandchild</title></topic></topics></children></topic></topics></children></topic></sheet></xmap-content>";
    for (file, data) in [("content.json", json), ("content.xml", xml)] {
        save(&dir, "map.xmind", zip(&[(file, data)]))?;
        let out = read(&dir, "map.xmind", json!({}))?;
        for expected in [
            "# Plan",
            "- Root",
            "  > Root note",
            "  - Child",
            "    - Grandchild",
        ] {
            assert!(text(&out).contains(expected), "{file}: {}", text(&out));
        }
    }
    Ok(())
}

#[test]
fn pptx_slide_order_lists_tables_links_and_notes() -> Result {
    let dir = Scratch::new()?;
    let slide = r#"<p:sld xmlns:p="urn:p" xmlns:a="urn:a" xmlns:r="urn:r"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>First slide</a:t></a:r></a:p><a:p><a:pPr lvl="1"><a:buChar char="*"/></a:pPr><a:r><a:t>Nested bullet</a:t></a:r></a:p><a:p><a:r><a:rPr><a:hlinkClick r:id="link"/></a:rPr><a:t>Reference</a:t></a:r></a:p></p:txBody></p:sp><a:tbl><a:tr><a:tc><a:p><a:r><a:t>Name</a:t></a:r></a:p></a:tc></a:tr><a:tr><a:tc><a:p><a:r><a:t>Alpha</a:t></a:r></a:p></a:tc></a:tr></a:tbl></p:spTree></p:cSld></p:sld>"#;
    save(
        &dir,
        "slides.pptx",
        zip(&[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="urn:p" xmlns:r="urn:r"><p:sldIdLst><p:sldId id="900" r:id="first"/><p:sldId id="901" r:id="second"/></p:sldIdLst></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="first" Target="slides/slide9.xml" Type="slide"/><Relationship Id="second" Target="slides/slide1.xml" Type="slide"/></Relationships>"#,
            ),
            ("ppt/slides/slide9.xml", slide),
            (
                "ppt/slides/slide1.xml",
                r#"<sld><p><r><t>Second slide</t></r></p></sld>"#,
            ),
            (
                "ppt/slides/_rels/slide9.xml.rels",
                r#"<Relationships><Relationship Id="notes" Target="../notesSlides/note1.xml" Type="notesSlide"/><Relationship Id="link" Target="https://example.org/slides" TargetMode="External" Type="hyperlink"/></Relationships>"#,
            ),
            (
                "ppt/notesSlides/note1.xml",
                "<notes><p><r><t>Speaker notes</t></r></p></notes>",
            ),
        ]),
    )?;
    let out = read(&dir, "slides.pptx", json!({}))?;
    for expected in [
        "# Slide 1",
        "First slide",
        "  - Nested bullet",
        "| Name |",
        "| Alpha |",
        "[Reference](https://example.org/slides)",
        "## Notes",
        "Speaker notes",
        "Second slide",
    ] {
        assert!(text(&out).contains(expected), "{expected}: {}", text(&out));
    }
    assert_eq!(chunks(&out)?[1]["locator"]["slide"], 2);
    let second = read(&dir, "slides.pptx", json!({"pages":"2"}))?;
    assert!(text(&second).contains("Second slide"));
    assert!(!text(&second).contains("First slide"));
    Ok(())
}

#[test]
fn libreoffice_odp_slides_and_speaker_notes() -> Result {
    let dir = Scratch::new()?;
    let flat = r#"<?xml version="1.0"?><office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.presentation"><office:body><office:presentation><draw:page draw:name="One"><draw:frame svg:width="20cm" svg:height="5cm"><draw:text-box><text:h text:outline-level="1">Opening slide</text:h><text:p>Slide body</text:p></draw:text-box></draw:frame><presentation:notes><draw:frame svg:width="20cm" svg:height="5cm"><draw:text-box><text:p>Speaker reminder</text:p></draw:text-box></draw:frame></presentation:notes></draw:page><draw:page draw:name="Two"><draw:frame svg:width="20cm" svg:height="5cm"><draw:text-box><text:p>Closing slide</text:p></draw:text-box></draw:frame></draw:page></office:presentation></office:body></office:document>"#;
    save(&dir, "deck.fodp", flat)?;
    office_convert(&dir, "deck.fodp", "odp")?;
    let out = read(&dir, "deck.odp", json!({}))?;
    for expected in [
        "# Slide 1",
        "Opening slide",
        "Slide body",
        "## Notes",
        "Speaker reminder",
        "Closing slide",
    ] {
        assert!(text(&out).contains(expected), "{expected}: {}", text(&out));
    }
    let second = read(&dir, "deck.odp", json!({"pages":"2"}))?;
    assert!(!text(&second).contains("Opening slide"));
    assert_eq!(chunks(&second)?[0]["locator"]["slide"], 2);
    Ok(())
}

#[test]
fn xlsx_ods_csv_tables_row_limits_and_cell_ranges() -> Result {
    let dir = Scratch::new()?;
    let xlsx = zip(&[
        (
            "xl/workbook.xml",
            r#"<workbook xmlns:r="urn:r"><sheets><sheet name="Data" r:id="one"/><sheet name="Other" r:id="two"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships><Relationship Id="one" Target="worksheets/sheet1.xml" Type="worksheet"/><Relationship Id="two" Target="worksheets/sheet2.xml" Type="worksheet"/></Relationships>"#,
        ),
        (
            "xl/sharedStrings.xml",
            "<sst><si><t>Name</t></si><si><r><t>Al</t></r><r><t>pha</t></r></si></sst>",
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="inlineStr"><is><t>Value</t></is></c></row><row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"><v>42</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>Tail</t></is></c><c r="B3"><v>99</v></c></row></sheetData></worksheet>"#,
        ),
        (
            "xl/worksheets/sheet2.xml",
            r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Other value</t></is></c></row></sheetData></worksheet>"#,
        ),
    ]);
    let ods = zip(&[(
        "content.xml",
        r#"<office:document-content xmlns:office="urn:office" xmlns:table="urn:table" xmlns:text="urn:text"><office:body><office:spreadsheet><table:table table:name="Data"><table:table-row><table:table-cell><text:p>Name</text:p></table:table-cell><table:table-cell><text:p>Value</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell><text:p>Alpha</text:p></table:table-cell><table:table-cell office:value="42"/></table:table-row><table:table-row><table:table-cell><text:p>Tail</text:p></table:table-cell><table:table-cell office:value="99"/></table:table-row><table:table-row table:number-rows-repeated="10000"><table:table-cell table:number-columns-repeated="1000"/></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#,
    )]);
    save(&dir, "in.xlsx", xlsx)?;
    save(&dir, "in.ods", ods)?;
    save(&dir, "in.csv", "Name,Value\r\nAlpha,42\r\nTail,99\r\n")?;
    for name in ["in.xlsx", "in.ods", "in.csv"] {
        let out = read(&dir, name, json!({"max_rows":2}))?;
        assert!(text(&out).contains("| Name | Value |"), "{}", text(&out));
        assert!(text(&out).contains("| Alpha | 42 |"));
        assert!(!text(&out).contains("Tail"));
        assert!(out.warnings.iter().any(|w| w.code == "read.rows_truncated"));
        assert_eq!(chunks(&out)?[0]["locator"]["range"], "A1:B2");
        let out = read(&dir, name, json!({"range":"B2:B3"}))?;
        assert!(text(&out).contains("| 42 |"));
        assert!(text(&out).contains("| 99 |"));
        assert!(!text(&out).contains("Alpha"));
        assert_eq!(chunks(&out)?[0]["locator"]["range"], "B2:B3");
    }
    let out = read(&dir, "in.xlsx", json!({"range":"Other!A1"}))?;
    assert!(text(&out).contains("Other value"));
    assert!(!text(&out).contains("Alpha"));
    save(
        &dir,
        "quoted.csv",
        "Name,Value\n\"A,B\",\"a \"\"quote\"\" and\nnext line\"\n",
    )?;
    let out = read(&dir, "quoted.csv", json!({}))?;
    assert!(text(&out).contains("A,B"));
    assert!(text(&out).contains("a \"quote\" and next line"));
    Ok(())
}

fn pdf_bytes(text_pages: usize, image: Option<&image::RgbImage>) -> Result<Vec<u8>> {
    use lopdf::{Document, Object, Stream, dictionary};
    let mut doc = Document::with_version("1.5");
    let pages = doc.new_object_id();
    let font =
        doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let mut kids = Vec::new();
    for index in 0..text_pages {
        let stream = format!(
            "BT /F1 24 Tf 50 740 Td (Heading {}) Tj ET\nBT /F1 12 Tf 50 700 Td (Body paragraph {}) Tj ET\nBT /F1 12 Tf 50 670 Td (More text here) Tj ET",
            index + 1,
            index + 1
        );
        let contents = doc.add_object(Stream::new(dictionary! {}, stream.into_bytes()));
        let page=doc.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Resources"=>dictionary!{"Font"=>dictionary!{"F1"=>font}},"Contents"=>contents});
        kids.push(Object::Reference(page));
    }
    if let Some(image) = image {
        let im=doc.add_object(Stream::new(dictionary!{"Type"=>"XObject","Subtype"=>"Image","Width"=>image.width(),"Height"=>image.height(),"BitsPerComponent"=>8,"ColorSpace"=>"DeviceRGB"},image.as_raw().clone()));
        let contents = doc.add_object(Stream::new(
            dictionary! {},
            b"q 750 0 0 100 0 0 cm /Im1 Do Q".to_vec(),
        ));
        let page=doc.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),750.into(),100.into()],"Resources"=>dictionary!{"XObject"=>dictionary!{"Im1"=>im}},"Contents"=>contents});
        kids.push(Object::Reference(page));
    }
    doc.objects.insert(
        pages,
        Object::Dictionary(dictionary! {"Type"=>"Pages","Count"=>kids.len() as i64,"Kids"=>kids}),
    );
    let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)?;
    Ok(bytes)
}

#[test]
fn pdf_page_locators_headings_and_page_selection() -> Result {
    let dir = Scratch::new()?;
    save(&dir, "in.pdf", pdf_bytes(2, None)?)?;
    let out = read(&dir, "in.pdf", json!({}))?;
    assert!(text(&out).contains("# Heading 1"), "{}", text(&out));
    assert!(!text(&out).contains("# **Heading 1**"), "{}", text(&out));
    assert!(text(&out).find("Body paragraph 1") < text(&out).find("Body paragraph 2"));
    assert_eq!(out.summary_params["pages"], 2);
    assert_eq!(chunks(&out)?[1]["locator"]["page"], 2);
    let out = read(&dir, "in.pdf", json!({"pages":"2"}))?;
    assert!(!text(&out).contains("paragraph 1"));
    assert!(text(&out).contains("paragraph 2"));
    assert_eq!(chunks(&out)?[0]["locator"]["page"], 2);
    Ok(())
}

#[test]
fn images_without_models_are_empty_with_explicit_warning() -> Result {
    let dir = Scratch::new()?;
    for ext in ["png", "jpg", "jpeg", "webp", "tif", "tiff"] {
        let name = format!("image.{ext}");
        save(&dir, &name, b"not decoded without models")?;
        let out = read(&dir, &name, json!({}))?;
        assert_eq!(text(&out), "");
        assert_eq!(out.warnings[0].code, "read.ocr_unavailable");
    }
    let image = image::RgbImage::from_pixel(40, 40, image::Rgb([255, 255, 255]));
    save(&dir, "scan.pdf", pdf_bytes(1, Some(&image))?)?;
    let out = read(&dir, "scan.pdf", json!({"pages":"2"}))?;
    assert_eq!(text(&out), "");
    assert!(
        out.warnings
            .iter()
            .any(|w| w.code == "read.ocr_unavailable")
    );
    let out = read(&dir, "scan.pdf", json!({"pages":"1"}))?;
    assert!(
        !out.warnings
            .iter()
            .any(|w| w.code == "read.ocr_unavailable")
    );
    Ok(())
}

#[test]
fn ocr_generated_english_images_and_scanned_pdf() -> Result {
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
    let font = crate::pdf::load_font(None)?;
    save(&dir, "font.ttf", font)?;
    let sentence = "The quick brown fox jumps over the lazy dog.";
    decode(
        &dir,
        "magick",
        &[
            "-size",
            "1500x200",
            "xc:white",
            "-font",
            "/work/font.ttf",
            "-pointsize",
            "48",
            "-fill",
            "black",
            "-annotate",
            "+25+110",
            sentence,
            "/work/scan.png",
        ],
    )?;
    for ext in ["jpg", "webp", "tiff"] {
        decode(
            &dir,
            "magick",
            &["/work/scan.png", &format!("/work/scan.{ext}")],
        )?;
    }
    for ext in ["png", "jpg", "webp", "tiff"] {
        let out = read_with(&dir, &format!("scan.{ext}"), json!({}), &opts)?;
        assert!(text(&out).contains(sentence), "{ext}: {}", text(&out));
        assert_eq!(chunks(&out)?[0]["locator"]["kind"], "image");
        assert!(out.warnings.iter().any(|w| w.code == "read.ocr"));
    }
    let image = image::open(dir.0.join("scan.png"))?.into_rgb8();
    save(&dir, "scan.pdf", pdf_bytes(0, Some(&image))?)?;
    let out = read_with(&dir, "scan.pdf", json!({}), &opts)?;
    assert!(text(&out).contains(sentence), "PDF: {}", text(&out));
    assert_eq!(chunks(&out)?[0]["locator"]["page"], 1);
    Ok(())
}

#[test]
fn pdf_links_and_tables_survive_reading() -> Result {
    use lopdf::{Object, dictionary};
    let dir = Scratch::new()?;
    let mut pdf = lopdf::Document::load_mem(&pdf_bytes(1, None)?)?;
    let page = *pdf.get_pages().values().next().ok_or("page")?;
    let annotation=pdf.add_object(dictionary! { "Type"=>"Annot", "Subtype"=>"Link", "Rect"=>vec![48.into(),668.into(),150.into(),684.into()], "A"=>dictionary! { "S"=>"URI", "URI"=>Object::string_literal("https://example.org/pdf") } });
    pdf.get_object_mut(page)?
        .as_dict_mut()?
        .set("Annots", vec![Object::Reference(annotation)]);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes)?;
    save(&dir, "links.pdf", bytes)?;
    let out = read(&dir, "links.pdf", json!({}))?;
    assert!(
        text(&out).contains("[More text here](https://example.org/pdf)"),
        "{}",
        text(&out)
    );
    run(
        &Input::Create {
            markdown: NOTE.into(),
            output: "table.pdf".into(),
        },
        &Options::new(&dir.0),
    )?;
    let out = read(&dir, "table.pdf", json!({}))?;
    assert!(text(&out).contains("| Name | Value |"), "{}", text(&out));
    assert!(text(&out).contains("| Alpha | 42 |"));
    assert!(
        pulldown_cmark::Parser::new(text(&out)).any(|event| matches!(
            event,
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::List(_))
        )),
        "PDF list structure: {}",
        text(&out)
    );
    Ok(())
}

#[test]
fn sandbox_has_no_network_or_access_to_caller_files() -> Result {
    let dir = Scratch::new()?;
    let outside = Scratch::new()?;
    save(&outside, "private.txt", "Private text")?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port().to_string();
    let secret = outside.0.join("private.txt");
    let script = "import os,socket,sys\nassert not os.path.exists(sys.argv[1])\ns=socket.socket();s.settimeout(0.5)\ntry:\n s.connect(('127.0.0.1',int(sys.argv[2])))\nexcept OSError:\n sys.exit(0)\nsys.exit(1)";
    decode(
        &dir,
        "python3",
        &["-c", script, secret.to_str().ok_or("path")?, &port],
    )?;
    Ok(())
}

#[test]
fn package_targets_cannot_escape_or_reference_a_url() -> Result {
    assert_eq!(
        super::xml::target("OPS/book.opf", "chapters/../first.xhtml")?,
        "OPS/first.xhtml"
    );
    assert!(super::xml::target("book.opf", "../first.xhtml").is_err());
    assert!(super::xml::target("book.opf", "%2e%2e/first.xhtml").is_err());
    assert_eq!(
        super::xml::target("OPS/book.opf", "Chapter%201.xhtml#top")?,
        "OPS/Chapter 1.xhtml"
    );
    assert!(super::xml::target("OPS/book.opf", "https://example.org/first.xhtml").is_err());
    Ok(())
}

#[test]
fn parser_and_ocr_errors_keep_their_causes() -> Result {
    let dir = Scratch::new()?;
    save(
        &dir,
        "bad.docx",
        zip(&[("word/document.xml", "<document><mismatch></document>")]),
    )?;
    let error = read(&dir, "bad.docx", json!({})).err().ok_or("error")?;
    assert!(error.source().is_some());
    std::fs::create_dir(dir.0.join("v5-det"))?;
    std::fs::create_dir(dir.0.join("v5-rec"))?;
    save(&dir, "v5-det/inference.onnx", b"invalid model")?;
    save(&dir, "v5-rec/inference.onnx", b"invalid model")?;
    save(&dir, "in.png", b"image")?;
    let opts = Options {
        ocr_models: Some(dir.0.clone()),
        ..Options::new(&dir.0)
    };
    let error = read_with(&dir, "in.png", json!({}), &opts)
        .err()
        .ok_or("error")?;
    assert!(error.source().is_some());
    assert!(!error.to_string().contains(dir.0.to_string_lossy().as_ref()));
    Ok(())
}

#[test]
fn rejects_excessive_html_depth() -> Result {
    let dir = Scratch::new()?;
    save(
        &dir,
        "deep.html",
        format!("{}Text{}", "<div>".repeat(140), "</div>".repeat(140)),
    )?;
    assert!(read(&dir, "deep.html", json!({})).is_err());
    Ok(())
}

#[test]
fn rejects_invalid_ranges_corrupt_packages_and_external_relationships() -> Result {
    let dir = Scratch::new()?;
    save(&dir, "in.txt", "hello")?;
    for extra in [
        json!({"range":"0"}),
        json!({"range":"2-1"}),
        json!({"pages":"1"}),
        json!({"pages":"1","range":"1"}),
        json!({"range":"4294967295"}),
    ] {
        assert!(read(&dir, "in.txt", extra).is_err());
    }
    save(&dir, "in.csv", "Name,Value\nAlpha,42\n")?;
    for extra in [
        json!({"range":"A0"}),
        json!({"range":"B2:A1"}),
        json!({"range":"Missing!A1"}),
        json!({"max_rows":0}),
        json!({"max_rows":100001}),
    ] {
        assert!(read(&dir, "in.csv", extra).is_err());
    }
    let mut bytes = zip(&[(
        "word/document.xml",
        "<document><body><p>Original</p></body></document>",
    )]);
    let at = bytes
        .windows(8)
        .position(|b| b == b"Original")
        .ok_or("fixture text")?;
    bytes[at] = b'X';
    save(&dir, "bad.docx", bytes)?;
    assert!(read(&dir, "bad.docx", json!({})).is_err());
    for target in ["../../escape.xml", "https://example.org/chapter"] {
        let package = format!(
            "<package><manifest><item id=\"one\" href=\"{target}\"/></manifest><spine><itemref idref=\"one\"/></spine></package>"
        );
        save(
            &dir,
            "bad.epub",
            zip(&[
                (
                    "META-INF/container.xml",
                    "<container><rootfile full-path=\"book.opf\"/></container>",
                ),
                ("book.opf", &package),
            ]),
        )?;
        assert!(read(&dir, "bad.epub", json!({})).is_err());
    }
    let xml = format!("{}x{}", "<n>".repeat(140), "</n>".repeat(140));
    save(&dir, "deep.xmind", zip(&[("content.xml", &xml)]))?;
    assert!(read(&dir, "deep.xmind", json!({})).is_err());
    Ok(())
}
