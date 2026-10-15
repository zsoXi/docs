//! End-to-end regressions for package preservation and consumer geometry.
use super::*;
use crate::read::raster::Scratch;
use lopdf::{Document, Object, dictionary};
use std::error::Error as StdError;
type Result<T = ()> = std::result::Result<T, Box<dyn StdError>>;

fn request(dir: &Scratch, value: serde_json::Value) -> Result<Output> {
    Ok(run(
        &serde_json::from_value(value)?,
        &Options::new(dir.0.clone()),
    )?)
}

pub(super) fn stream_pdf(content: &[u8]) -> Result<Document> {
    let mut pdf = Document::with_version("1.5");
    let pages = pdf.new_object_id();
    let stream = pdf.add_object(lopdf::Stream::new(dictionary! {}, content.to_vec()));
    let page = pdf.add_object(dictionary! {"Type"=>"Page", "Parent"=>pages,
    "MediaBox"=>vec![0.into(),0.into(),612.into(),792.into()], "Contents"=>stream,
    "Resources"=>dictionary!{}});
    pdf.objects.insert(
        pages,
        dictionary! {"Type"=>"Pages", "Kids"=>vec![page.into()], "Count"=>1}.into(),
    );
    let root = pdf.add_object(dictionary! {"Type"=>"Catalog", "Pages"=>pages});
    pdf.trailer.set("Root", root);
    Ok(pdf)
}

#[test]
fn r13_docx_edit_preserves_every_unrelated_part_and_markup() -> Result {
    let dir = Scratch::new()?;
    // An independent ZIP producer adds parts that the object-model reader
    // cannot round-trip. The image and hyperlink are real DOCX relationships.
    let mut doc = docx_rs::Docx::new().add_paragraph(
        docx_rs::Paragraph::new()
            .add_run(docx_rs::Run::new().add_text("Northstar "))
            .add_run(docx_rs::Run::new().bold().add_text("Analytics Ltd"))
            .add_hyperlink(
                docx_rs::Hyperlink::new(
                    "https://example.org/report",
                    docx_rs::HyperlinkType::External,
                )
                .add_run(docx_rs::Run::new().add_text(" report")),
            ),
    );
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        10,
        10,
        image::Rgb([20, 90, 160]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)?;
    doc = doc.add_paragraph(
        docx_rs::Paragraph::new()
            .add_run(docx_rs::Run::new().add_image(docx_rs::Pic::new(png.get_ref()))),
    );
    std::fs::write(dir.0.join("source.docx"), docx::pack(doc)?)?;
    let script = r#"
import zipfile, pathlib
p=pathlib.Path('source.docx')
with zipfile.ZipFile(p) as z: parts={n:z.read(n) for n in z.namelist()}
ns='http://schemas.openxmlformats.org/wordprocessingml/2006/main'
parts['word/header1.xml']=f'<w:hdr xmlns:w="{ns}"><w:p><w:r><w:t>Header untouched</w:t></w:r></w:p></w:hdr>'.encode()
parts['word/footnotes.xml']=f'<w:footnotes xmlns:w="{ns}"><w:footnote w:id="1"><w:p><w:r><w:t>Footnote untouched</w:t></w:r></w:p></w:footnote></w:footnotes>'.encode()
parts['word/comments.xml']=f'<w:comments xmlns:w="{ns}"><w:comment w:id="0" w:author="Reviewer"><w:p><w:r><w:t>Comment untouched</w:t></w:r></w:p></w:comment></w:comments>'.encode()
doc=parts['word/document.xml'].decode()
doc=doc.replace('</w:sectPr>', '<w:headerReference w:type="default" r:id="rIdHeader"/></w:sectPr>')
doc=doc.replace('<w:body>', '<w:body><!-- preserved markup -->', 1)
doc=doc.replace('</w:body>', '<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>Fee 100</w:t><w:footnoteReference w:id="1"/></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p></w:body>')
parts['word/document.xml']=doc.encode()
rels=parts['word/_rels/document.xml.rels'].decode()
for kind,target,rid in [('header','header1.xml','rIdHeader'),('footnotes','footnotes.xml','rIdFootnote'),('comments','comments.xml','rIdComment')]:
 rels=rels.replace('</Relationships>',f'<Relationship Id="{rid}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/></Relationships>')
parts['word/_rels/document.xml.rels']=rels.encode()
ct=parts['[Content_Types].xml'].decode()
for kind in ['header','footnotes','comments']:
 path='header1' if kind=='header' else kind
 ct=ct.replace('</Types>',f'<Override PartName="/word/{path}.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.{kind}+xml"/></Types>')
parts['[Content_Types].xml']=ct.encode()
with zipfile.ZipFile(p,'w',compression=zipfile.ZIP_DEFLATED) as z:
 for n,b in parts.items(): z.writestr(n,b)
"#;
    let status = std::process::Command::new("python3")
        .args(["-c", script])
        .current_dir(&dir.0)
        .status()?;
    assert!(status.success());
    for inplace in [false, true] {
        std::fs::copy(dir.0.join("source.docx"), dir.0.join("work.docx"))?;
        let first = if inplace { "work.docx" } else { "first.docx" };
        let second = if inplace { "work.docx" } else { "second.docx" };
        let result = request(
            &dir,
            serde_json::json!({"op":"replace","input":"work.docx","output":first,"find":"Northstar Analytics Ltd","replace":"Cedar & Sons <Ltd>"}),
        )?;
        assert_eq!(result.summary_params.get("count"), Some(&1.into()));
        let first_check = std::process::Command::new("python3").args(["-c", "import zipfile,sys; a=zipfile.ZipFile('source.docx'); b=zipfile.ZipFile(sys.argv[1]); assert a.namelist()==b.namelist(); [(None if n=='word/document.xml' else (a.read(n)==b.read(n) or sys.exit('changed part: '+n))) for n in a.namelist()]", first]).current_dir(&dir.0).output()?;
        assert!(first_check.status.success(), "{:?}", first_check);
        request(
            &dir,
            serde_json::json!({"op":"replace","input":first,"output":second,"find":"100","replace":"200"}),
        )?;
        let check = r#"
import zipfile,sys,xml.etree.ElementTree as E
with zipfile.ZipFile('source.docx') as a,zipfile.ZipFile(sys.argv[1]) as b:
 assert a.namelist()==b.namelist()
 for n in a.namelist():
  if n!='word/document.xml': assert a.read(n)==b.read(n),n
 old=a.read('word/document.xml').decode(); new=b.read('word/document.xml').decode()
 assert '<!-- preserved markup -->' in new
 assert '<w:b' in new
 text=''.join(E.fromstring(new).itertext())
 assert 'Cedar & Sons <Ltd>' in text and 'Fee 200' in text, text
 assert 'Northstar' not in text
 expected=old.replace('Northstar ', 'Cedar &amp; Sons &lt;Ltd&gt;').replace('Analytics Ltd','').replace('100','200')
 # Only the two edited text elements may gain whitespace preservation.
 assert new.replace(' xml:space="preserve"','') == expected.replace(' xml:space="preserve"','')
 assert b.testzip() is None
"#;
        let out = std::process::Command::new("python3")
            .args(["-c", check, second])
            .current_dir(&dir.0)
            .output()?;
        assert!(out.status.success(), "{:?}", out);
    }
    Ok(())
}

#[test]
fn r13_create_pdf_image_survives_resource_representations() -> Result {
    let dir = Scratch::new()?;
    image::RgbImage::from_pixel(20, 12, image::Rgb([10, 180, 30])).save(dir.0.join("chart.png"))?;
    request(
        &dir,
        serde_json::json!({"op":"create","markdown":"# Report\n\n![Chart](chart.png)","output":"source.pdf"}),
    )?;
    let original = Document::load(dir.0.join("source.pdf"))?;
    for variant in 0..6 {
        let mut pdf = original.clone();
        let page = pdf.get_pages().into_values().next().ok_or("page")?;
        let resource_value = pdf.get_dictionary(page)?.get(b"Resources")?.clone();
        let mut resources = pdf.dereference(&resource_value)?.1.as_dict()?.clone();
        if variant % 2 == 1 {
            let value = resources.get(b"XObject")?.clone();
            let objects = pdf.dereference(&value)?.1.clone();
            let id = pdf.add_object(objects);
            resources.set("XObject", id);
        }
        let value = if variant >= 4 {
            Object::Reference(pdf.add_object(resources))
        } else {
            Object::Dictionary(resources)
        };
        if variant >= 2 {
            let parent = pdf.get_dictionary(page)?.get(b"Parent")?.as_reference()?;
            pdf.get_dictionary_mut(page)?.remove(b"Resources");
            pdf.get_dictionary_mut(parent)?.set("Resources", value);
        } else {
            pdf.get_dictionary_mut(page)?.set("Resources", value);
        }
        pdf.save(dir.0.join("input.pdf"))?;
        let result = request(
            &dir,
            serde_json::json!({"op":"convert","input":"input.pdf","output":"out.docx"}),
        )?;
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.code == "pdfdocx.image_at_end"),
            "{variant}: {:?}",
            result.warnings
        );
        let bytes = std::fs::read(dir.0.join("out.docx"))?;
        let parsed = docx::parse(&bytes)?;
        assert_eq!(parsed.images.len(), 1, "variant {variant}");
        let image = image::load_from_memory(&parsed.images[0].2.0)?.to_rgb8();
        assert_eq!(image.dimensions(), (20, 12));
        assert!(image.pixels().all(|p| p.0 == [10, 180, 30]));
        if variant == 0
            && let Some(path) = std::env::var_os("DOCS_TEST_ARTIFACTS")
        {
            let out = PathBuf::from(path).join("raster-report");
            std::fs::create_dir_all(&out)?;
            std::fs::copy(dir.0.join("source.pdf"), out.join("source.pdf"))?;
            std::fs::write(out.join("converted.docx"), &bytes)?;
        }
        assert!(
            parsed
                .images
                .iter()
                .any(|(_, _, data, _)| !data.0.is_empty())
        );
    }
    Ok(())
}

#[test]
fn r13_docx_numeric_alignment_is_measured_in_native_pdf() -> Result {
    let dir = Scratch::new()?;
    let mut doc = docx_rs::Docx::new()
        .default_fonts(
            docx_rs::RunFonts::new()
                .ascii("Noto Sans")
                .hi_ansi("Noto Sans"),
        )
        .default_size(22)
        .default_line_spacing(docx_rs::LineSpacing::new().line(240).after(120));
    for align in [
        docx_rs::AlignmentType::Left,
        docx_rs::AlignmentType::Center,
        docx_rs::AlignmentType::Right,
    ] {
        let rows = ["1.00", "12345.00"]
            .iter()
            .map(|value| {
                docx_rs::TableRow::new(vec![
                    docx_rs::TableCell::new()
                        .width(6000, docx_rs::WidthType::Dxa)
                        .add_paragraph(
                            docx_rs::Paragraph::new()
                                .align(align)
                                .add_run(docx_rs::Run::new().add_text(*value)),
                        ),
                ])
            })
            .collect();
        doc = doc.add_table(docx_rs::Table::new(rows).set_grid(vec![6000]));
    }
    std::fs::write(dir.0.join("invoice.docx"), docx::pack(doc)?)?;
    request(
        &dir,
        serde_json::json!({"op":"convert","input":"invoice.docx","output":"invoice.pdf"}),
    )?;
    let pdf = Document::load(dir.0.join("invoice.pdf"))?;
    let page = pdf.get_pages().into_values().next().ok_or("page")?;
    let lines = pdfedit::positioned_lines(&pdf, page)?;
    let short: Vec<_> = lines.iter().filter(|l| l.text.trim() == "1.00").collect();
    let long: Vec<_> = lines
        .iter()
        .filter(|l| l.text.trim() == "12345.00")
        .collect();
    assert_eq!(short.len(), 3, "{lines:?}");
    assert_eq!(long.len(), 3);
    for (i, factor) in [0.0, 0.5, 1.0].into_iter().enumerate() {
        let delta = short[i].x - long[i].x;
        let expected = (long[i].width - short[i].width) * factor;
        assert!(
            (delta - expected).abs() < 1.0,
            "alignment {i}: delta={delta} expected={expected}; {lines:?}"
        );
    }
    assert!(
        short[2].x > short[0].x + 100.0,
        "right alignment was ignored"
    );
    request(
        &dir,
        serde_json::json!({"op":"create","markdown":"# Invoice\n\n| Description | Qty | Amount |\n|---|---:|---:|\n| Analysis | 10 | 1,000.00 |\n| Review | 2 | 500.00 |\n| Total | 12 | 1,500.00 |", "output":"created.docx"}),
    )?;
    request(
        &dir,
        serde_json::json!({"op":"convert","input":"created.docx","output":"created.pdf"}),
    )?;
    let pdf = Document::load(dir.0.join("created.pdf"))?;
    let page = pdf.get_pages().into_values().next().ok_or("page")?;
    let lines = pdfedit::positioned_lines(&pdf, page)?;
    let amounts: Vec<_> = lines.iter().filter(|l| l.text.contains(".00")).collect();
    assert_eq!(amounts.len(), 3, "{lines:?}");
    let right = amounts[0].x + amounts[0].width;
    let bytes = std::fs::read(dir.0.join("created.docx"))?;
    let setup = docxhtml::convert(&bytes, &docx::parse(&bytes)?)?.setup;
    let expected_right = (setup.width_mm - setup.margin_right_mm) * 72.0 / 25.4 - 5.4;
    assert!(
        (right - expected_right).abs() < 1.0,
        "right={right} cell edge={expected_right}; {lines:?}"
    );
    assert!(
        amounts.iter().all(|l| (l.x + l.width - right).abs() < 1.0),
        "{lines:?}"
    );
    let bbox = std::process::Command::new("pdftotext")
        .args(["-bbox", "created.pdf", "-"])
        .current_dir(&dir.0)
        .output()?;
    assert!(bbox.status.success(), "{bbox:?}");
    let root = docxxml::parse(&bbox.stdout)?;
    fn right_edges(node: &docxxml::Element, out: &mut Vec<f64>) -> Result {
        if node.name == "word" && node.text.ends_with(".00") {
            out.push(node.attr("xMax").ok_or("xMax")?.parse()?);
        }
        for child in &node.children {
            right_edges(child, out)?;
        }
        Ok(())
    }
    let mut edges = Vec::new();
    right_edges(&root, &mut edges)?;
    assert_eq!(edges.len(), 3);
    assert!(
        edges.iter().all(|x| (x - edges[0]).abs() < 1.0),
        "Poppler: {edges:?}"
    );
    if let Some(path) = std::env::var_os("DOCS_TEST_ARTIFACTS") {
        let out = PathBuf::from(path).join("native-invoice");
        std::fs::create_dir_all(&out)?;
        for name in ["created.docx", "created.pdf"] {
            std::fs::copy(dir.0.join(name), out.join(name))?;
        }
        std::fs::write(out.join("bbox.html"), bbox.stdout)?;
    }
    Ok(())
}

#[test]
fn r13_docx_first_only_no_match_and_parallel_updates() -> Result {
    let dir = Scratch::new()?;
    request(
        &dir,
        serde_json::json!({"op":"create","markdown":"Client 100 Client 100", "output":"state.docx"}),
    )?;
    let before = std::fs::read(dir.0.join("state.docx"))?;
    request(
        &dir,
        serde_json::json!({"op":"replace","input":"state.docx","output":"state.docx","find":"absent","replace":"nothing"}),
    )?;
    assert_eq!(std::fs::read(dir.0.join("state.docx"))?, before);
    let result = request(
        &dir,
        serde_json::json!({"op":"replace","input":"state.docx","output":"state.docx","find":"100","replace":"200","all":false}),
    )?;
    assert_eq!(result.summary_params.get("count"), Some(&1.into()));
    assert!(
        docx::read_text(&std::fs::read(dir.0.join("state.docx"))?)?
            .contains("Client 200 Client 100")
    );
    let handles: Vec<_> = [("Client", "Vendor"), ("200", "300")].into_iter().map(|(find, replacement)| {
        let base = dir.0.clone();
        std::thread::spawn(move || run(&serde_json::from_value(serde_json::json!({"op":"replace","input":"state.docx","output":"state.docx","find":find,"replace":replacement}))?, &Options::new(base)))
    }).collect();
    for handle in handles {
        handle.join().map_err(|_| "update thread failed")??;
    }
    let text = docx::read_text(&std::fs::read(dir.0.join("state.docx"))?)?;
    assert!(text.contains("Vendor 300 Vendor 100"), "{text}");
    Ok(())
}

#[test]
fn r13_fallback_extraction_retains_typed_source() -> Result {
    let dir = Scratch::new()?;
    let mut pdf = stream_pdf(b"")?;
    let encryption = pdf.add_object(dictionary! {"Filter"=>"Standard", "V"=>1, "R"=>2, "P"=>-4,
    "O"=>Object::String(vec![0;32], lopdf::StringFormat::Hexadecimal),
    "U"=>Object::String(vec![0;32], lopdf::StringFormat::Hexadecimal)});
    pdf.trailer.set("Encrypt", encryption);
    pdf.trailer.set(
        "ID",
        vec![
            Object::string_literal("test-id"),
            Object::string_literal("test-id"),
        ],
    );
    pdf.save(dir.0.join("locked.pdf"))?;
    let err = request(
        &dir,
        serde_json::json!({"op":"convert","input":"locked.pdf","output":"out.txt"}),
    )
    .err()
    .ok_or("expected failure")?;
    let err = err.downcast_ref::<Error>().ok_or("domain error")?;
    assert_eq!(err.code(), MessageCode::PdfTextExtract);
    assert!(
        StdError::source(err)
            .ok_or("missing extractor source")?
            .is::<pdf_extract::OutputError>()
    );
    assert!(!err.to_string().contains("test-id"));
    assert!(!dir.0.join("out.txt").exists());
    Ok(())
}
