//! Consumer regressions for inherited pages, bounded streams and page programs.
use super::*;
use crate::read::raster::Scratch;
use lopdf::{Document, Object, Stream, dictionary};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn request(dir: &Scratch, value: serde_json::Value) -> Result<Output> {
    Ok(run(&serde_json::from_value(value)?, &Options::new(&dir.0))?)
}

fn fixture() -> Result<Document> {
    let mut doc =
        crate::round13_tests::stream_pdf(b"BT /F1 12 Tf 40 200 Td (Invoice total: ) Tj ET")?;
    let font =
        doc.add_object(dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica"});
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    doc.get_dictionary_mut(page)?
        .set("Resources", dictionary! {"Font"=>dictionary! {"F1"=>font}});
    Ok(doc)
}

fn poppler(dir: &Scratch, input: &str) -> Result<String> {
    let out = std::process::Command::new("pdftotext")
        .args([input, "-"])
        .current_dir(&dir.0)
        .output()?;
    assert!(out.status.success(), "{out:?}");
    Ok(String::from_utf8(out.stdout)?)
}

#[test]
fn r14_merge_and_split_materialize_inherited_page_attributes() -> Result {
    let dir = Scratch::new()?;
    for variant in 0..3 {
        let mut doc = fixture()?;
        let page = doc.get_pages().into_values().next().ok_or("page")?;
        let parent = doc.get_dictionary(page)?.get(b"Parent")?.as_reference()?;
        let ancestor = if variant == 0 {
            parent
        } else {
            let nested = doc.add_object(dictionary! {"Type"=>"Pages", "Parent"=>parent, "Kids"=>vec![page.into()], "Count"=>1});
            doc.get_dictionary_mut(page)?.set("Parent", nested);
            doc.get_dictionary_mut(parent)?
                .set("Kids", vec![nested.into()]);
            nested
        };
        let resources = doc
            .get_dictionary_mut(page)?
            .remove(b"Resources")
            .ok_or("resources")?;
        let resources = if variant == 2 {
            Object::Reference(doc.add_object(resources))
        } else {
            resources
        };
        doc.get_dictionary_mut(page)?.remove(b"MediaBox");
        let tree = doc.get_dictionary_mut(ancestor)?;
        tree.set("Resources", resources);
        tree.set("MediaBox", vec![0.into(), 0.into(), 400.into(), 300.into()]);
        tree.set(
            "CropBox",
            vec![10.into(), 10.into(), 390.into(), 290.into()],
        );
        tree.set("Rotate", 90);
        doc.save(dir.0.join(format!("in{variant}.pdf")))?;
    }
    let mut originals = Vec::new();
    for index in 0..3 {
        let result = std::process::Command::new("pdftoppm")
            .args([
                "-singlefile",
                "-cropbox",
                "-r",
                "36",
                "-png",
                &format!("in{index}.pdf"),
                "original",
            ])
            .current_dir(&dir.0)
            .output()?;
        assert!(result.status.success(), "{result:?}");
        originals.push(std::fs::read(dir.0.join("original.png"))?);
    }
    for output in ["merged.pdf", "in0.pdf"] {
        request(
            &dir,
            serde_json::json!({"op":"merge", "inputs":["in0.pdf","in1.pdf","in2.pdf"], "output":output}),
        )?;
        let text = poppler(&dir, output)?;
        assert_eq!(text.matches("Invoice total:").count(), 3, "{text}");
        let merged = Document::load(dir.0.join(output))?;
        assert_eq!(merged.get_pages().len(), 3);
        for (index, original) in originals.iter().enumerate() {
            let number = (index + 1).to_string();
            let result = std::process::Command::new("pdftoppm")
                .args([
                    "-singlefile",
                    "-cropbox",
                    "-f",
                    &number,
                    "-l",
                    &number,
                    "-r",
                    "36",
                    "-png",
                    output,
                    "merged-page",
                ])
                .current_dir(&dir.0)
                .output()?;
            assert!(result.status.success(), "{result:?}");
            assert_eq!(
                &std::fs::read(dir.0.join("merged-page.png"))?,
                original,
                "render changed: {output} page {number}"
            );
        }
        for page in merged.get_pages().into_values() {
            let dict = merged.get_dictionary(page)?;
            assert_eq!(dict.get(b"MediaBox")?.as_array()?[2].as_i64()?, 400);
            assert_eq!(dict.get(b"Rotate")?.as_i64()?, 90);
            assert_eq!(dict.get(b"CropBox")?.as_array()?[0].as_i64()?, 10);
            assert!(dict.has(b"Resources"));
        }
        request(
            &dir,
            serde_json::json!({"op":"split", "input":output, "pages":"2", "output":"split.pdf"}),
        )?;
        assert_eq!(
            poppler(&dir, "split.pdf")?
                .matches("Invoice total:")
                .count(),
            1
        );
    }
    Ok(())
}

#[test]
fn r14_document_stream_budget_covers_unused_streams_and_fonts() -> Result {
    let dir = Scratch::new()?;
    let mut doc = fixture()?;
    let mut bomb = Stream::new(dictionary! {}, vec![b' '; 9 * 1024 * 1024]);
    bomb.compress()?;
    doc.add_object(bomb);
    doc.save(dir.0.join("bomb.pdf"))?;
    for value in [
        serde_json::json!({"op":"read", "input":"bomb.pdf"}),
        serde_json::json!({"op":"convert", "input":"bomb.pdf", "output":"out.docx"}),
        serde_json::json!({"op":"replace", "input":"bomb.pdf", "output":"bomb.pdf", "find":"Invoice", "replace":"Bill"}),
        serde_json::json!({"op":"merge", "inputs":["bomb.pdf"], "output":"out.pdf"}),
        serde_json::json!({"op":"split", "input":"bomb.pdf", "pages":"1", "output":"out.pdf"}),
    ] {
        let before = std::fs::read(dir.0.join("bomb.pdf"))?;
        let error = run(
            &serde_json::from_value(value.clone())?,
            &Options::new(&dir.0),
        )
        .err()
        .ok_or("accepted bomb")?;
        assert_eq!(
            error.code(),
            MessageCode::PdfContentLimit,
            "{value}: {error}"
        );
        assert_eq!(std::fs::read(dir.0.join("bomb.pdf"))?, before);
    }
    Ok(())
}

#[test]
fn r14_contents_share_text_state_for_read_convert_and_replace() -> Result {
    let dir = Scratch::new()?;
    let mut doc = fixture()?;
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    let first = doc.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 40 200 Td (Invoice total: ) Tj".to_vec(),
    ));
    let second = doc.add_object(Stream::new(dictionary! {}, b"(1500) Tj ET".to_vec()));
    doc.get_dictionary_mut(page)?
        .set("Contents", vec![first.into(), second.into()]);
    doc.save(dir.0.join("source.pdf"))?;
    let output = request(&dir, serde_json::json!({"op":"read", "input":"source.pdf"}))?;
    assert!(output.text.ok_or("text")?.contains("1500"));
    request(
        &dir,
        serde_json::json!({"op":"convert", "input":"source.pdf", "output":"out.docx"}),
    )?;
    assert!(docx::read_text(&std::fs::read(dir.0.join("out.docx"))?)?.contains("1500"));
    let result = request(
        &dir,
        serde_json::json!({"op":"replace", "input":"source.pdf", "output":"source.pdf", "find":"1500", "replace":"1750"}),
    )?;
    assert_eq!(result.summary_params.get("count"), Some(&1.into()));
    assert!(poppler(&dir, "source.pdf")?.contains("1750"));
    Ok(())
}

#[test]
fn r14_forms_text_matrix_resources_and_private_edits() -> Result {
    let dir = Scratch::new()?;
    for only_form in [false, true] {
        let mut doc = fixture()?;
        let page = doc.get_pages().into_values().next().ok_or("page")?;
        let font =
            doc.add_object(dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Courier"});
        let form = doc.add_object(Stream::new(dictionary! {
            "Type"=>"XObject", "Subtype"=>"Form", "BBox"=>vec![0.into(),0.into(),200.into(),100.into()],
            "Matrix"=>vec![2.into(),0.into(),0.into(),2.into(),10.into(),20.into()],
            "Resources"=>dictionary! {"Font"=>dictionary! {"F1"=>font, "DokuSub_0"=>font}},
        }, b"BT /DokuSub_0 8 Tf 10 70 Td (Context) Tj ET BT /F1 10 Tf 10 20 Td (1500) Tj ET".to_vec()));
        let form = if only_form {
            doc.add_object(Stream::new(dictionary! {
                "Type"=>"XObject", "Subtype"=>"Form", "BBox"=>vec![0.into(),0.into(),300.into(),250.into()],
                "Matrix"=>vec![1.into(),0.into(),0.into(),1.into(),5.into(),5.into()],
                "Resources"=>dictionary! {"XObject"=>dictionary! {"Child"=>form}},
            }, b"/Child Do".to_vec()))
        } else {
            form
        };
        let content = if only_form {
            b"/Fm Do q 1 0 0 1 200 0 cm /Fm Do Q".as_slice()
        } else {
            b"BT /F1 12 Tf 40 200 Td (Invoice total:) Tj ET /Fm Do q 1 0 0 1 200 0 cm /Fm Do Q"
                .as_slice()
        };
        let stream = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
        let mut res = doc
            .get_dictionary(page)?
            .get(b"Resources")?
            .as_dict()?
            .clone();
        res.set("XObject", dictionary! {"Fm"=>form});
        doc.get_dictionary_mut(page)?.set("Contents", stream);
        doc.get_dictionary_mut(page)?.set("Resources", res);
        doc.save(dir.0.join("form.pdf"))?;
        let output = request(&dir, serde_json::json!({"op":"read", "input":"form.pdf"}))?;
        assert_eq!(output.text.ok_or("text")?.matches("1500").count(), 2);
        assert!(
            !output
                .warnings
                .iter()
                .any(|w| w.code == "read.ocr_unavailable")
        );
        let lines = pdfedit::positioned_lines(&doc, page)?;
        let amounts: Vec<_> = lines.iter().filter(|l| l.text == "1500").collect();
        assert_eq!(amounts.len(), 2);
        let offset = if only_form { 5.0 } else { 0.0 };
        assert!(
            (amounts[0].x - 30.0 - offset).abs() < 0.01
                && (amounts[0].y - 60.0 - offset).abs() < 0.01
        );
        assert!((amounts[0].size - 20.0).abs() < 0.01);
        assert!(amounts[0].font.contains("Courier"));
        request(
            &dir,
            serde_json::json!({"op":"convert", "input":"form.pdf", "output":"form.docx"}),
        )?;
        assert_eq!(
            docx::read_text(&std::fs::read(dir.0.join("form.docx"))?)?
                .matches("1500")
                .count(),
            2
        );
        let result = request(
            &dir,
            serde_json::json!({"op":"replace", "input":"form.pdf", "output":"first.pdf", "find":"1500", "replace":"1750", "all":false}),
        )?;
        assert_eq!(result.summary_params.get("count"), Some(&1.into()));
        let text = poppler(&dir, "first.pdf")?;
        assert_eq!(text.matches("1750").count(), 1, "{text}");
        assert_eq!(text.matches("1500").count(), 1, "{text}");
        let result = request(
            &dir,
            serde_json::json!({"op":"replace", "input":"form.pdf", "output":"form.pdf", "find":"1500", "replace":"1750"}),
        )?;
        assert_eq!(result.summary_params.get("count"), Some(&2.into()));
        assert_eq!(poppler(&dir, "form.pdf")?.matches("1750").count(), 2);
        let result = request(
            &dir,
            serde_json::json!({"op":"replace", "input":"form.pdf", "output":"form.pdf", "find":"1750", "replace":"Zażółć", "all":false}),
        )?;
        assert_eq!(result.summary_params.get("count"), Some(&1.into()));
        let text = poppler(&dir, "form.pdf")?;
        assert_eq!(text.matches("Zażółć").count(), 1, "{text}");
        assert_eq!(text.matches("1750").count(), 1, "{text}");
        assert_eq!(text.matches("Context").count(), 2, "{text}");
    }
    Ok(())
}

#[test]
fn r14_large_markdown_image_fits_one_printable_page() -> Result {
    let dir = Scratch::new()?;
    let mut source = fixture()?;
    let page = source.get_pages().into_values().next().ok_or("page")?;
    let content = source.add_object(Stream::new(dictionary! {}, b"BT /F1 22 Tf 24 450 Td (Quarterly report - complete sentence and final column retained) Tj ET BT /F1 16 Tf 24 400 Td (Region) Tj 300 0 Td (Revenue) Tj 300 0 Td (Margin) Tj ET BT /F1 16 Tf 24 350 Td (North) Tj 300 0 Td (1500) Tj 300 0 Td (25 percent) Tj ET BT /F1 16 Tf 24 300 Td (South) Tj 300 0 Td (1750) Tj 300 0 Td (30 percent) Tj ET 20 280 740 140 re S 20 330 m 760 330 l S 20 380 m 760 380 l S 300 280 m 300 420 l S 600 280 m 600 420 l S".to_vec()));
    source
        .get_dictionary_mut(page)?
        .set("MediaBox", vec![0.into(), 0.into(), 800.into(), 500.into()]);
    source.get_dictionary_mut(page)?.set("Contents", content);
    source.save(dir.0.join("scan-source.pdf"))?;
    let out = std::process::Command::new("pdftoppm")
        .args([
            "-singlefile",
            "-scale-to-x",
            "1600",
            "-scale-to-y",
            "1000",
            "-png",
            "scan-source.pdf",
            "scan-source",
        ])
        .current_dir(&dir.0)
        .output()?;
    assert!(out.status.success(), "{out:?}");
    let mut image = image::open(dir.0.join("scan-source.png"))?.to_rgb8();
    assert_eq!(image.dimensions(), (1600, 1000));
    for y in 0..1000 {
        for x in 1560..1600 {
            image.put_pixel(x, y, image::Rgb([240, 0, 0]));
        }
    }
    image.save(dir.0.join("scan.png"))?;
    request(
        &dir,
        serde_json::json!({"op":"create", "markdown":"![Scan](scan.png)", "output":"scan.pdf"}),
    )?;
    let doc = Document::load(dir.0.join("scan.pdf"))?;
    assert_eq!(doc.get_pages().len(), 1);
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    let bounds = pdfedit::page_bounds(&doc, page)?;
    let ink = bounds.ink.ok_or("ink")?.0;
    let margin = 20.0 * 72.0 / 25.4;
    assert!(
        ink[0] >= margin - 1.0 && ink[2] <= bounds.media[2] - margin + 1.0,
        "{ink:?}"
    );
    assert!(
        ((ink[2] - ink[0]) / (ink[3] - ink[1]) - 1.6).abs() < 0.02,
        "{ink:?}"
    );
    let out = std::process::Command::new("pdftoppm")
        .args(["-singlefile", "-r", "72", "-png", "scan.pdf", "render"])
        .current_dir(&dir.0)
        .output()?;
    assert!(out.status.success(), "{out:?}");
    let rendered = image::open(dir.0.join("render.png"))?.to_rgb8();
    assert!(
        rendered
            .pixels()
            .filter(|p| p[0] > 180 && p[1] < 40 && p[2] < 40)
            .count()
            > 1000,
        "right image edge was clipped"
    );
    assert!(
        rendered
            .enumerate_pixels()
            .filter(|(x, _, p)| *x > 400 && p[0] < 100 && p[1] < 100 && p[2] < 100)
            .count()
            > 200,
        "text and table in the rightmost column disappeared"
    );
    if let Some(path) = std::env::var_os("DOCS_TEST_ARTIFACTS") {
        std::fs::create_dir_all(&path)?;
        std::fs::copy(
            dir.0.join("render.png"),
            PathBuf::from(path).join("large-image.png"),
        )?;
    }
    Ok(())
}
