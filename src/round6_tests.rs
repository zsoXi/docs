//! Ink-derived margins and page geometry; every fixture is built in memory.
use super::*;
use lopdf::{Document, Object, Stream, dictionary};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture(contents: &[&str]) -> (Document, Vec<lopdf::ObjectId>) {
    let mut doc = Document::with_version("1.5");
    let font=doc.add_object(dictionary!{"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica","Encoding"=>"WinAnsiEncoding"});
    let image=doc.add_object(Stream::new(dictionary!{"Type"=>"XObject","Subtype"=>"Image","Width"=>1,"Height"=>1,"BitsPerComponent"=>8,"ColorSpace"=>"DeviceRGB"},vec![0,0,0]));
    let form=doc.add_object(Stream::new(dictionary!{"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),100.into(),100.into()],"Matrix"=>vec![1.into(),0.into(),0.into(),1.into(),5.into(),7.into()]},b"10 20 20 30 re f".to_vec()));
    let pages = doc.new_object_id();
    let mut ids = Vec::new();
    for text in contents {
        let content = doc.add_object(Stream::new(dictionary! {}, text.as_bytes().to_vec()));
        ids.push(doc.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"Contents"=>content}));
    }
    doc.objects.insert(pages,Object::Dictionary(dictionary!{"Type"=>"Pages","Kids"=>ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),"Count"=>ids.len() as i64,"MediaBox"=>vec![10.into(),20.into(),610.into(),820.into()],"Resources"=>dictionary!{"Font"=>dictionary!{"F1"=>font},"XObject"=>dictionary!{"Im"=>image,"Fm"=>form}}}));
    let root = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
    doc.trailer.set("Root", root);
    (doc, ids)
}
fn close(actual: [f64; 4], expected: [f64; 4]) {
    for (a, e) in actual.into_iter().zip(expected) {
        assert!((a - e).abs() < 0.026, "{actual:?} != {expected:?}");
    }
}
#[test]
fn r6_all_four_margins_include_painted_graphics() -> TestResult {
    let (doc, pages) =
        fixture(&["30 100 500 650 re f BT /F1 12 Tf 1 0 0 1 80 600 Tm (Text) Tj ET"]);
    close(
        pdfedit::page_bounds(&doc, pages[0])?.margins(),
        [20.0, 70.0, 80.0, 80.0],
    );
    let bytes = pdfdocx::build(&doc, &pages)?.data;
    let xml = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("no document")?)?;
    let margin = xml.path(&["body", "sectPr", "pgMar"]).ok_or("no margins")?;
    for (key, value) in [
        ("left", "400"),
        ("top", "1400"),
        ("right", "1600"),
        ("bottom", "1600"),
    ] {
        assert_eq!(margin.attr(key), Some(value));
    }
    Ok(())
}
#[test]
fn r6_unpainted_paths_and_unused_images_do_not_change_margins() -> TestResult {
    let (doc, pages) = fixture(&["10 20 600 800 re W n 50 150 300 500 re f 10 20 600 800 re n"]);
    close(
        pdfedit::page_bounds(&doc, pages[0])?.margins(),
        [40.0, 170.0, 260.0, 130.0],
    );
    Ok(())
}
#[test]
fn r6_transformed_image_and_form_extents() -> TestResult {
    let (doc, pages) = fixture(&["q 0 80 -40 0 200 300 cm /Im Do Q q 2 0 0 3 50 80 cm /Fm Do Q"]);
    // Image [160,300,200,380], Form [80,161,120,251].
    close(
        pdfedit::page_bounds(&doc, pages[0])?.margins(),
        [70.0, 440.0, 410.0, 141.0],
    );
    Ok(())
}
#[test]
fn r6_cubic_extrema_not_control_polygon() -> TestResult {
    let (doc, pages) = fixture(&["100 200 m 100 600 500 600 500 200 c h f"]);
    // Maximum y is 500, at t=.5, not the controls' y=600.
    close(
        pdfedit::page_bounds(&doc, pages[0])?.margins(),
        [90.0, 320.0, 110.0, 180.0],
    );
    Ok(())
}
#[test]
fn r6_butt_and_round_stroke_caps_and_width() -> TestResult {
    let (doc, pages) = fixture(&[
        "10 w 100 300 m 400 300 l S",
        "10 w 1 J 100 300 m 400 300 l S",
    ]);
    close(
        pdfedit::page_bounds(&doc, pages[0])?.margins(),
        [90.0, 515.0, 210.0, 275.0],
    );
    close(
        pdfedit::page_bounds(&doc, pages[1])?.margins(),
        [85.0, 515.0, 205.0, 275.0],
    );
    Ok(())
}
#[test]
fn r6_rectangular_clip_and_graphics_save_restore() -> TestResult {
    let (doc, pages) =
        fixture(&["q 100 200 200 300 re W n 10 20 600 800 re f Q 400 600 20 20 re f"]);
    close(
        pdfedit::page_bounds(&doc, pages[0])?.margins(),
        [90.0, 200.0, 190.0, 180.0],
    );
    Ok(())
}
#[test]
fn r6_each_page_has_its_own_section_even_without_text() -> TestResult {
    let (mut doc, pages) = fixture(&["30 100 500 650 re f", "100 200 300 400 re f"]);
    doc.get_dictionary_mut(pages[1])?
        .set("MediaBox", vec![0.into(), 0.into(), 500.into(), 700.into()]);
    let bytes = pdfdocx::build(&doc, &pages)?.data;
    let root = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("no document")?)?;
    fn collect<'a>(node: &'a docxxml::Element, found: &mut Vec<&'a docxxml::Element>) {
        if node.name == "sectPr" {
            found.push(node);
        }
        for c in &node.children {
            collect(c, found);
        }
    }
    let mut sections = Vec::new();
    collect(&root, &mut sections);
    assert_eq!(sections.len(), 2);
    assert_eq!(
        sections[0].child("pgSz").ok_or("size")?.attr("w"),
        Some("12000")
    );
    assert_eq!(
        sections[1].child("pgSz").ok_or("size")?.attr("h"),
        Some("14000")
    );
    assert_eq!(
        sections[1].child("pgMar").ok_or("margins")?.attr("bottom"),
        Some("4000")
    );
    Ok(())
}
#[test]
fn r6_text_bounds_use_glyph_ink_not_font_size_or_trailing_spaces() -> TestResult {
    let font = pdf::load_font(None)?;
    let bytes = pdf::html_to_pdf(
        "<html><body style=\"margin:0;font-family:'Noto Sans';font-size:11pt\"><p>Hg   </p></body></html>",
        &font,
    )?;
    let doc = Document::load_mem(&bytes)?;
    let page = doc.get_pages().into_values().next().ok_or("no page")?;
    let lines = pdfedit::positioned_lines(&doc, page)?;
    let line = lines.first().ok_or("no text")?;
    let ink = pdfedit::page_bounds(&doc, page)?.ink.ok_or("no ink")?.0;
    let face = ttf_parser::Face::parse(&font, 0)?;
    let h = face
        .glyph_bounding_box(face.glyph_index('H').ok_or("H")?)
        .ok_or("H ink")?;
    let g = face
        .glyph_bounding_box(face.glyph_index('g').ok_or("g")?)
        .ok_or("g ink")?;
    let em = f64::from(face.units_per_em());
    assert!((ink[0] - line.x - f64::from(h.x_min) / em * line.size).abs() < 0.026);
    assert!((ink[1] - line.y - f64::from(g.y_min) / em * line.size).abs() < 0.026);
    assert!((ink[3] - line.y - f64::from(h.y_max) / em * line.size).abs() < 0.026);
    assert!(ink[2] < line.x + line.width);
    Ok(())
}
#[test]
fn r6_invisible_and_clipped_text_has_no_ink() -> TestResult {
    let (doc, pages) = fixture(&[
        "BT /F1 12 Tf 3 Tr 1 0 0 1 10 800 Tm (Invisible) Tj ET q 200 200 100 100 re W n BT /F1 12 Tf 0 Tr 1 0 0 1 20 700 Tm (Clipped) Tj ET Q 100 400 50 50 re f",
    ]);
    close(
        pdfedit::page_bounds(&doc, pages[0])?.margins(),
        [90.0, 370.0, 460.0, 380.0],
    );
    Ok(())
}

#[test]
fn r6_deep_forms_are_bounded_and_report_estimates() -> TestResult {
    let (mut doc, pages) = fixture(&["/Deep Do"]);
    let mut next=doc.add_object(Stream::new(dictionary!{"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),600.into(),800.into()]},b"100 200 100 200 re f".to_vec()));
    for _ in 0..33 {
        next=doc.add_object(Stream::new(dictionary!{"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Resources"=>dictionary!{"XObject"=>dictionary!{"Next"=>next}}},b"/Next Do".to_vec()));
    }
    doc.get_dictionary_mut(pages[0])?.set(
        "Resources",
        dictionary! {"XObject"=>dictionary!{"Deep"=>next}},
    );
    let bounds = pdfedit::page_bounds(&doc, pages[0])?;
    assert!(bounds.ink.is_none());
    assert!(bounds.approximate);
    let built = pdfdocx::build(&doc, &pages)?;
    let warning = built
        .warnings
        .iter()
        .find(|w| w.code == messages::MessageCode::PdfDocxBoundsApproximate)
        .ok_or("no warning")?;
    assert_eq!(warning.params.get("page"), Some(&serde_json::json!(1)));
    Ok(())
}

#[test]
fn r6_repeated_paragraphs_keep_distinct_positions_and_line_breaks() -> TestResult {
    let (doc, pages) = fixture(&[
        "BT /F1 12 Tf 1 0 0 1 50 750 Tm (Same) Tj 1 0 0 1 50 700 Tm (Same) Tj 1 0 0 1 50 686 Tm (Next row) Tj ET",
    ]);
    let bytes = pdfdocx::build(&doc, &pages)?.data;
    let root = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("no document")?)?;
    let paragraphs: Vec<_> = root
        .child("body")
        .ok_or("body")?
        .children
        .iter()
        .filter(|n| n.name == "p")
        .collect();
    assert_eq!(paragraphs.len(), 2);
    let frame = |index: usize| {
        paragraphs[index]
            .path(&["pPr", "framePr"])
            .and_then(|n| n.attr("y"))
    };
    assert_ne!(frame(0), frame(1));
    assert!(format!("{:?}", paragraphs[1]).contains("name: \"br\""));
    Ok(())
}

#[test]
fn r6_non_finite_paint_coordinates_return_an_error() -> TestResult {
    // PDF numbers do not use exponent notation; overflow a decimal token.
    let content = format!("{} 200 100 200 re f", "9".repeat(400));
    let (doc, pages) = fixture(&[&content]);
    assert!(matches!(
        pdfedit::page_bounds(&doc, pages[0]),
        Err(Error::Pdf(lopdf::Error::Parse(
            lopdf::ParseError::InvalidContentStream
        )))
    ));
    Ok(())
}

#[test]
fn r6_malformed_form_matrix_returns_an_error() -> TestResult {
    let (mut doc, pages) = fixture(&["/Fm Do"]);
    let parent = doc
        .get_dictionary(pages[0])?
        .get(b"Parent")?
        .as_reference()?;
    let id = doc
        .get_dictionary(parent)?
        .get(b"Resources")?
        .as_dict()?
        .get(b"XObject")?
        .as_dict()?
        .get(b"Fm")?
        .as_reference()?;
    doc.get_object_mut(id)?.as_stream_mut()?.dict.set(
        "Matrix",
        vec![1.into(), 0.into(), 0.into(), 1.into(), 5.into()],
    );
    assert!(matches!(
        pdfedit::page_bounds(&doc, pages[0]),
        Err(Error::Pdf(lopdf::Error::Parse(
            lopdf::ParseError::InvalidContentStream
        )))
    ));
    Ok(())
}

#[test]
fn r6_cyclic_inherited_resources_return_an_error() -> TestResult {
    let (mut doc, pages) = fixture(&["100 200 100 200 re f"]);
    let parent = doc
        .get_dictionary(pages[0])?
        .get(b"Parent")?
        .as_reference()?;
    doc.get_dictionary_mut(parent)?.remove(b"Resources");
    doc.get_dictionary_mut(parent)?.set("Parent", parent);
    let error = pdfedit::page_bounds(&doc, pages[0])
        .err()
        .ok_or("cycle accepted")?;
    assert!(matches!(error, Error::Pdf(lopdf::Error::ReferenceCycle(_))));
    Ok(())
}

#[test]
fn r6_pdf_table_keeps_heights_shading_and_page_positions() -> TestResult {
    let (doc, pages) = fixture(&[
        "0.9 0.8 0.7 rg 50 680 300 20 re f 0 g 50 700 m 350 700 l S 50 680 m 350 680 l S 50 660 m 350 660 l S 50 660 m 50 700 l S 200 660 m 200 700 l S 350 660 m 350 700 l S BT /F1 12 Tf 1 0 0 1 55 685 Tm (Header) Tj 1 0 0 1 205 685 Tm (Value) Tj 1 0 0 1 55 665 Tm (Row) Tj 1 0 0 1 205 665 Tm (42) Tj ET BT /F1 18 Tf 1 0 0 1 50 750 Tm (Title) Tj ET",
    ]);
    let bytes = pdfdocx::build(&doc, &pages)?.data;
    let root = docxxml::parse(&docxxml::entry(&bytes, "word/document.xml")?.ok_or("no document")?)?;
    let body = root.child("body").ok_or("body")?;
    let table = body.child("tbl").ok_or("table")?;
    let position = table.path(&["tblPr", "tblpPr"]).ok_or("table position")?;
    assert_eq!(position.attr("horzAnchor"), Some("page"));
    assert_eq!(position.attr("tblpX"), Some("800"));
    let row = table.child("tr").ok_or("row")?;
    assert_eq!(
        row.path(&["trPr", "trHeight"]).ok_or("height")?.attr("val"),
        Some("400")
    );
    assert_eq!(
        row.path(&["tc", "tcPr", "shd"])
            .ok_or("shading")?
            .attr("fill"),
        Some("E6CCB3")
    );
    assert!(body.path(&["p", "pPr", "framePr"]).is_some());
    Ok(())
}

#[test]
fn r6_docx_paragraph_strut_and_table_spacing_are_resolved() -> TestResult {
    use docx_rs::*;
    let docx = Docx::new()
        .default_fonts(RunFonts::new().ascii("Liberation Sans"))
        .default_size(40)
        .add_paragraph(Paragraph::new().add_run(Run::new().add_text("Heading")))
        .add_table(Table::new(vec![TableRow::new(vec![
            TableCell::new().add_paragraph(
                Paragraph::new()
                    .line_spacing(LineSpacing::new().after(120))
                    .add_run(Run::new().add_text("Cell")),
            ),
        ])]));
    let bytes = crate::docx::pack(docx)?;
    let converted = docxhtml::convert(&bytes, &crate::docx::parse(&bytes)?)?;
    // The paragraph owns the default face and size, rather than nesting an
    // identical span whose strut adds a second baseline inside the box.
    assert!(
        converted.body.contains("font-size:20pt"),
        "{}",
        converted.body
    );
    let (_, height) = pdf::family_metrics("Liberation Sans")?.ok_or("missing font")?;
    assert!(
        converted.body.contains(&format!("line-height:{height};")),
        "{}",
        converted.body
    );
    assert!(!converted.body.contains("<span"), "{}", converted.body);
    assert!(
        converted.body.contains("padding-bottom:6pt;"),
        "{}",
        converted.body
    );
    Ok(())
}
