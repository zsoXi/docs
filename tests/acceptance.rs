//! Scenario acceptance uses the same source bytes for native and independent consumers.
mod support;
use serde_json::json;
use support::*;

#[test]
fn s1_markdown_to_docx() -> Result {
    let dir = Dir::new()?;
    dir.native_docx()?;
    let xml = zip_xml(&dir, "report.docx", "word/document.xml")?;
    for value in [
        "Heading1",
        "Heading2",
        "Heading3",
        "<w:b",
        "<w:i",
        "<w:tbl",
        "<w:numPr",
        "cx=\"2286000\"",
        "cy=\"857250\"",
    ] {
        assert!(xml.contains(value), "missing {value}: {xml}");
    }
    assert!(
        zip_xml(&dir, "report.docx", "word/_rels/document.xml.rels")?
            .contains("https://example.org/audit")
    );
    assert!(zip_xml(&dir, "report.docx", "word/numbering.xml")?.contains("w:val=\"3\""));
    content(&dir.repeat(json!({"op":"read","input":"report.docx"}))?)?;
    dir.repeat(json!({"op":"create","markdown":"# Polish sample\n\nZażółć gęślą jaźń. Łódź.","output":"polish.docx"}))?;
    assert!(
        dir.read("polish.docx")?
            .text
            .ok_or("text")?
            .contains("Zażółć gęślą jaźń")
    );
    Ok(())
}

#[test]
fn s2_markdown_to_pdf() -> Result {
    let dir = Dir::new()?;
    dir.native_pdf()?;
    content(&dir.repeat(json!({"op":"read","input":"report.pdf"}))?)?;
    pdf_checks(&dir, "report.pdf", Some(1), Some((240., 90.)))?;
    dir.repeat(json!({"op":"create","markdown":"# Polish sample\n\nZażółć gęślą jaźń. Łódź.","output":"polish.pdf"}))?;
    assert!(
        dir.read("polish.pdf")?
            .text
            .ok_or("text")?
            .contains("Zażółć gęślą jaźń")
    );
    Ok(())
}

#[test]
fn s3_docx_to_pdf() -> Result {
    let dir = Dir::new()?;
    dir.native_docx()?;
    dir.lo()?;
    for source in ["report", "independent"] {
        let output = format!("{source}-converted.pdf");
        dir.repeat(json!({"op":"convert","input":format!("{source}.docx"),"output":output}))?;
        content(&dir.read(&output)?)?;
        pdf_checks(&dir, &output, None, Some((240., 90.)))?;
    }
    Ok(())
}

#[test]
fn s4_read_docx_to_markdown() -> Result {
    let dir = Dir::new()?;
    dir.native_docx()?;
    dir.lo()?;
    for input in ["report.docx", "independent.docx"] {
        let out = dir.repeat(json!({"op":"read","input":input,"locators":true}))?;
        let text = content(&out)?;
        for required in [
            "# Quarterly Business Report",
            "## Financial results",
            "### Revenue outlook",
            "**approved revenue**",
            "*reviewed forecasts*",
            "3. Approve budget",
            "| Revenue | $1,234.50 |",
        ] {
            assert!(text.contains(required), "{input}: {required}: {text}");
        }
    }
    Ok(())
}

#[test]
fn s5_read_text_pdf_to_markdown() -> Result {
    let dir = Dir::new()?;
    dir.native_pdf()?;
    dir.lo()?;
    for input in ["report.pdf", "independent.pdf"] {
        let out = dir.repeat(json!({"op":"read","input":input,"locators":true}))?;
        content(&out)?;
        assert!(
            out.text
                .as_deref()
                .is_some_and(|s| s.contains("https://example.org/audit"))
        );
    }
    Ok(())
}

#[test]
fn s6_read_scanned_pdf_and_png_with_confidence() -> Result {
    let Some(models) = std::env::var_os("DOCS_TEST_OCR_MODELS") else {
        eprintln!("SKIP S6: set DOCS_TEST_OCR_MODELS, DOCS_TEST_OCR_CORPUS and ORT_DYLIB_PATH");
        return Ok(());
    };
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        eprintln!("SKIP S6: set ORT_DYLIB_PATH");
        return Ok(());
    }
    let models = std::path::PathBuf::from(models);
    let corpus = std::env::var_os("DOCS_TEST_OCR_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or(models.parent().ok_or("model parent")?.join("corpus"));
    let dir = Dir::new()?;
    let opts = docs::Options {
        ocr_models: Some(models),
        ocr_worker: Some(env!("CARGO_BIN_EXE_docs-ocr").into()),
        ..docs::Options::new(&dir.0)
    };
    let mut scans = lopdf::Document::with_version("1.5");
    let parent = scans.new_object_id();
    let mut kids = Vec::new();
    for (i, name) in ["print-en-00.png", "columns-en-00.png", "print-pl-01.png"]
        .iter()
        .enumerate()
    {
        std::fs::copy(corpus.join(name), dir.0.join(name))?;
        let request: docs::Input = serde_json::from_value(
            json!({"op":"read","input":name,"locators":true,"max_chars":100000}),
        )?;
        let a = docs::run(&request, &opts)?;
        let b = docs::run(&request, &opts)?;
        assert_eq!(
            serde_json::to_value(&a)?,
            serde_json::to_value(&b)?,
            "S6 I2 {name}"
        );
        let text = a.text.as_deref().ok_or("OCR text")?;
        assert!(text.len() > 100, "{name}: {a:?}");
        if i == 0 {
            assert!(
                text.contains("The quick brown fox jumps over the lazy dog."),
                "{text}"
            );
        }
        let data = a.data.ok_or("OCR data")?;
        let lines = data["ocr"][0]["lines"].as_array().ok_or("lines")?;
        assert!(!lines.is_empty());
        for line in lines {
            assert!(
                line["confidence"]
                    .as_f64()
                    .is_some_and(|v| (0.0..=1.0).contains(&v))
            );
        }
        if i == 1 {
            assert!(
                text.find("1.12").ok_or("left end")? < text.find("2.1").ok_or("right start")?,
                "{text}"
            );
        }
        if i == 2 {
            assert!(text.contains("Łódź"), "{text}");
        }
        let image = image::open(dir.0.join(name))?.into_rgb8();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).encode_image(&image)?;
        let im = scans.add_object(lopdf::Stream::new(lopdf::dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>image.width(),"Height"=>image.height(),"ColorSpace"=>"DeviceRGB","BitsPerComponent"=>8,"Filter"=>"DCTDecode"},jpeg));
        let stream = scans.add_object(lopdf::Stream::new(
            lopdf::dictionary! {},
            b"q 600 0 0 800 0 0 cm /Im Do Q".to_vec(),
        ));
        kids.push(lopdf::Object::Reference(scans.add_object(lopdf::dictionary!{"Type"=>"Page","Parent"=>parent,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Resources"=>lopdf::dictionary!{"XObject"=>lopdf::dictionary!{"Im"=>im}},"Contents"=>stream})));
    }
    scans.objects.insert(
        parent,
        lopdf::Object::Dictionary(lopdf::dictionary! {"Type"=>"Pages","Kids"=>kids,"Count"=>3}),
    );
    let catalog = scans.add_object(lopdf::dictionary! {"Type"=>"Catalog","Pages"=>parent});
    scans.trailer.set("Root", catalog);
    scans.save(dir.0.join("scans.pdf"))?;
    let request = serde_json::from_value(
        json!({"op":"read","input":"scans.pdf","locators":true,"max_chars":100000}),
    )?;
    let first = docs::run(&request, &opts)?;
    assert_eq!(
        first.data.as_ref().ok_or("scan data")?["ocr"]
            .as_array()
            .ok_or("pages")?
            .len(),
        3
    );
    let text = first.text.as_deref().ok_or("scan text")?;
    assert!(
        text.contains("Łódź") && text.contains("The quick brown fox jumps over the lazy dog."),
        "{text}"
    );
    assert!(
        text.find("1.12").ok_or("scan left end")? < text.find("2.1").ok_or("scan right start")?,
        "{text}"
    );
    assert_eq!(
        serde_json::to_value(&first)?,
        serde_json::to_value(docs::run(&request, &opts)?)?
    );
    Ok(())
}

#[test]
fn docx_thematic_break_uses_plain_hyphens() -> Result {
    let dir = Dir::new()?;
    dir.repeat(json!({"op":"create","markdown":"Above\n\n---\n\nBelow","output":"rule.docx"}))?;
    let xml = zip_xml(&dir, "rule.docx", "word/document.xml")?;
    assert!(xml.contains("-------------"), "{xml}");
    assert!(!xml.contains('\u{2014}'), "{xml}");
    Ok(())
}
