//! Browser defaults, run boundaries and explicit bounds diagnostics.
use super::*;
use lopdf::{Document, Object, Stream, dictionary};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture(text: &str) -> (Document, lopdf::ObjectId) {
    let mut doc = Document::with_version("1.5");
    let regular = doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica","Encoding"=>"WinAnsiEncoding"});
    let bold = doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica-Bold","Encoding"=>"WinAnsiEncoding"});
    let image = doc.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>1,"Height"=>1,"BitsPerComponent"=>8,"ColorSpace"=>"DeviceRGB"}, vec![0,0,0]));
    let pages = doc.new_object_id();
    let content = doc.add_object(Stream::new(dictionary! {}, text.as_bytes().to_vec()));
    let page = doc.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"Contents"=>content,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Resources"=>dictionary! {"Font"=>dictionary! {"F1"=>regular,"F2"=>bold},"XObject"=>dictionary! {"Im"=>image},"ExtGState"=>dictionary! {"Plain"=>dictionary! {"ca"=>1,"CA"=>1,"BM"=>"Normal","SMask"=>"None"},"Alpha"=>dictionary! {"ca"=>0.5}}}});
    doc.objects.insert(
        pages,
        Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>vec![page.into()],"Count"=>1}),
    );
    let root = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
    doc.trailer.set("Root", root);
    (doc, page)
}
fn warnings(doc: &Document, page: lopdf::ObjectId) -> Result<Vec<messages::Msg>, Error> {
    Ok(pdfdocx::build(doc, &[page])?
        .warnings
        .into_iter()
        .filter(|w| w.code == MessageCode::PdfDocxBoundsApproximate)
        .collect())
}

#[test]
fn r7_run_boundary_spaces_are_not_duplicated() -> TestResult {
    for boundary in ["", " "] {
        let (doc, page) = fixture(&format!(
            "BT /F1 12 Tf 1 0 0 1 50 700 Tm (With company ) Tj /F2 12 Tf ({boundary}Kowalski Sp. z o.o. ) Tj /F1 12 Tf ({boundary}and customer.) Tj 1 0 0 1 50 660 Tm (Price is ) Tj /F2 12 Tf ({boundary}1500 net) Tj ET"
        ));
        let bytes = pdfdocx::build(&doc, &[page])?.data;
        let text = docx::read_text(&bytes)?;
        assert!(
            text.contains("With company Kowalski Sp. z o.o. and customer."),
            "{text:?}"
        );
        assert!(text.contains("Price is 1500 net"), "{text:?}");
        assert!(!text.contains("  "), "{text:?}");
        let xml =
            String::from_utf8(docxxml::entry(&bytes, "word/document.xml")?.ok_or("document")?)?;
        assert!(xml.contains("<w:b"));
        assert!(xml.contains("w:framePr"));
    }
    Ok(())
}

#[test]
fn r7_typical_paint_has_exact_bounds_without_warning() -> TestResult {
    for content in [
        "BT /F1 12 Tf 1 0 0 1 50 700 Tm (Ordinary text) Tj ET",
        "2 w 50 100 300 400 re S",
        "2 w 50 100 m 350 100 l S 50 120 m 350 120 l S 50 140 m 350 140 l S 50 100 m 50 140 l S 350 100 m 350 140 l S",
        "100 100 m 100 300 300 300 300 100 c h f",
        "q 100 0 0 80 100 200 cm /Im Do Q",
        "/Plain gs 50 100 300 400 re f",
        "[3 2] 0 d [] 0 d 50 100 m 350 100 l S",
        "q 50 100 300 400 re W n 60 110 20 20 re f Q",
        "BT /F1 12 Tf 1 0 0 1 50 700 Tm q /F2 12 Tf Q /F1 12 Tf (Reset state) Tj ET",
    ] {
        let (doc, page) = fixture(content);
        let found = warnings(&doc, page)?;
        assert!(found.is_empty(), "{content}: {found:?}");
    }
    Ok(())
}

#[test]
fn r7_unsupported_bounds_name_the_operator_exactly_once() -> TestResult {
    for (content, operator) in [
        ("/Alpha gs /Alpha gs 50 100 300 400 re f", "gs"),
        ("/Gradient sh", "sh"),
        ("/Pattern cs /Tile scn 50 100 300 400 re f", "scn"),
        ("/Pattern CS /Tile SCN 50 100 300 400 re S", "SCN"),
        ("[3 2] 0 d 50 100 m 350 100 l S", "S"),
        ("0 w 50 100 m 350 100 l S", "S"),
        (
            "50 100 m 150 300 l 350 100 l h W n 50 100 300 400 re f",
            "W",
        ),
        ("50 100 m 150 300 l 350 100 l S", "S"),
        ("100 100 m 100 300 300 300 300 100 c S", "S"),
        ("/Missing Do", "Do"),
        (
            "BT /F1 12 Tf 0.8 0.6 -0.6 0.8 100 600 Tm (Rotated) Tj ET",
            "Tj",
        ),
        ("BX 1 FuturePaint EX", "FuturePaint"),
        ("BT /F1 12 Tf 1 Tr 1 0 0 1 50 700 Tm (Outline) Tj ET", "Tr"),
        ("BT /F1 12 Tf 7 Tr 1 0 0 1 50 700 Tm (Clip) Tj ET", "Tr"),
        (
            "BT /F1 12 Tf 1 0 0 1 50 700 Tm q /F2 12 Tf Q (Restored font) Tj ET",
            "Q",
        ),
        (
            "BT /F1 12 Tf 1 0 0 1 50 700 Tm q 5 Tc Q (Restored spacing) Tj ET",
            "Q",
        ),
        (
            "BT /F1 12 Tf 1 0 0 1 50 700 Tm q 30 TL Q T* (Restored leading) Tj ET",
            "Q",
        ),
    ] {
        let (doc, page) = fixture(content);
        let found = warnings(&doc, page)?;
        assert_eq!(found.len(), 1, "{content}: {found:?}");
        assert_eq!(found[0].params.get("page"), Some(&serde_json::json!(1)));
        assert_eq!(
            found[0].params.get("operator"),
            Some(&serde_json::json!(operator))
        );
    }
    Ok(())
}

#[test]
fn r7_nondefault_page_geometry_and_annotations_are_diagnosed() -> TestResult {
    for (key, value) in [
        ("Rotate", Object::Integer(90)),
        ("UserUnit", Object::Integer(2)),
        (
            "CropBox",
            vec![0.into(), 0.into(), 300.into(), 400.into()].into(),
        ),
        ("Annots", vec![Object::Null].into()),
    ] {
        let (mut doc, page) = fixture("50 100 300 400 re f");
        assert!(warnings(&doc, page)?.is_empty());
        doc.get_dictionary_mut(page)?.set(key, value);
        let found = warnings(&doc, page)?;
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].params.get("operator"),
            Some(&serde_json::json!(key))
        );
    }
    Ok(())
}

#[test]
fn r7_image_masks_are_explicitly_approximate() -> TestResult {
    let (mut doc, page) = fixture("q 100 0 0 80 100 200 cm /Im Do Q");
    assert!(warnings(&doc, page)?.is_empty());
    let image = doc
        .get_dictionary(page)?
        .get(b"Resources")?
        .as_dict()?
        .get(b"XObject")?
        .as_dict()?
        .get(b"Im")?
        .as_reference()?;
    doc.get_object_mut(image)?.as_stream_mut()?.dict.set(
        "Mask",
        vec![0.into(), 0.into(), 0.into(), 0.into(), 0.into(), 0.into()],
    );
    let found = warnings(&doc, page)?;
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].params.get("operator"),
        Some(&serde_json::json!("Do"))
    );
    Ok(())
}

#[test]
fn r7_form_inherits_simple_stroke_geometry() -> TestResult {
    let (mut doc, page) = fixture("6 w /Fm Do");
    let form = doc.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),600.into(),800.into()]}, b"100 100 m 200 100 l S".to_vec()));
    doc.get_dictionary_mut(page)?
        .get_mut(b"Resources")?
        .as_dict_mut()?
        .get_mut(b"XObject")?
        .as_dict_mut()?
        .set("Fm", form);
    let bounds = pdfedit::page_bounds(&doc, page)?;
    assert_eq!(bounds.ink.ok_or("ink")?.0, [100.0, 97.0, 200.0, 103.0]);
    assert!(warnings(&doc, page)?.is_empty());
    Ok(())
}

fn render_html(html: &str) -> Result<Document, Error> {
    Document::load_mem(&htmlpdf::render(
        html,
        &pdf::load_font(None)?,
        &Default::default(),
    )?)
    .map_err(Error::Pdf)
}
fn lines(doc: &Document) -> Result<Vec<pdfedit::PositionedLine>, Error> {
    doc.get_pages()
        .into_values()
        .map(|p| pdfedit::positioned_lines(doc, p))
        .collect::<Result<Vec<_>, _>>()
        .map(|v| v.into_iter().flatten().collect())
}
#[test]
fn r7_html_browser_defaults_headings_quote_and_pre() -> TestResult {
    let doc = render_html(
        "<html><head></head><body><h1>One</h1><h2>Two</h2><h3>Three</h3><h4>Four</h4><h5>Five</h5><h6>Six</h6><p>Body</p><blockquote>Quote</blockquote><pre>first\n  second</pre><p><code>Code</code></p></body></html>",
    )?;
    let all = lines(&doc)?;
    let line = |text: &str| {
        all.iter()
            .find(|l| l.text.trim() == text)
            .ok_or_else(|| format!("missing {text}: {all:?}"))
    };
    for (text, size) in [
        ("One", 24.0),
        ("Two", 18.0),
        ("Three", 14.04),
        ("Four", 12.0),
        ("Five", 9.96),
        ("Six", 8.04),
    ] {
        assert!(
            (line(text)?.size - size).abs() < 0.03,
            "{text}: {:?}",
            line(text)?
        );
        assert!(line(text)?.font.to_ascii_lowercase().contains("bold"));
    }
    assert!((line("Quote")?.x - line("Body")?.x - 30.0).abs() < 0.1);
    assert!(line("first")?.y > line("second")?.y);
    assert!(
        line("second")?.text.starts_with("  "),
        "{:?}",
        line("second")?
    );
    assert!((line("Code")?.size - 9.75).abs() < 0.03);
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    let bounds = pdfedit::page_bounds(&doc, page)?;
    assert!((bounds.media[2] - 612.0).abs() < 0.02);
    assert!((bounds.media[3] - 792.0).abs() < 0.02);
    Ok(())
}

#[test]
fn r7_html_author_css_overrides_defaults_and_keeps_borders() -> TestResult {
    let html = "<html><head><style>@page { size:A4; margin:20mm; } body { margin:0; font-family:'Noto Sans'; font-size:11pt; } h1 {font-size:20pt;margin:0 0 12pt;} p {margin:0 0 6pt;} table {border-collapse:collapse;width:330pt;} td,th {border:1px solid #333;padding:4pt;text-align:left;}</style></head><body><h1>Title</h1><p>Paragraph</p><table><tr><th>Header</th><th>Value</th></tr><tr><td>Cell</td><td>42</td></tr></table></body></html>";
    let doc = render_html(html)?;
    let all = lines(&doc)?;
    let title = all.iter().find(|l| l.text == "Title").ok_or("title")?;
    let body = all.iter().find(|l| l.text == "Paragraph").ok_or("body")?;
    assert!((title.size - 20.0).abs() < 0.03);
    assert!((body.size - 11.0).abs() < 0.03);
    assert!((title.x - 20.0 * 72.0 / 25.4).abs() < 0.03);
    let compact = lines(&render_html(&html.replace("margin:0 0 12pt", "margin:0"))?)?;
    let compact_title = compact
        .iter()
        .find(|l| l.text == "Title")
        .ok_or("compact title")?;
    let compact_body = compact
        .iter()
        .find(|l| l.text == "Paragraph")
        .ok_or("compact body")?;
    assert!(((title.y - body.y) - (compact_title.y - compact_body.y) - 12.0).abs() < 0.03);
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    let bounds = pdfedit::page_bounds(&doc, page)?;
    assert!((bounds.media[2] - 210.0 * 72.0 / 25.4).abs() < 0.03);
    let segments = pdfedit::page_segments(&doc, page)?;
    assert!(segments.iter().any(|s| (s.x1 - s.x0).abs() > 100.0));
    assert!(segments.iter().any(|s| (s.y1 - s.y0).abs() > 10.0));
    assert!(warnings(&doc, page)?.is_empty());
    Ok(())
}
