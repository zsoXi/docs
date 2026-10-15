//! Ligature extraction, Word compatibility and operator-level bounds diagnostics.
use super::*;
use lopdf::{Document, Object, Stream, dictionary};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture(content: &str) -> (Document, lopdf::ObjectId, lopdf::ObjectId) {
    let mut doc = Document::with_version("1.5");
    let font = doc.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica","Encoding"=>"WinAnsiEncoding"});
    let pages = doc.new_object_id();
    let stream = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page = doc.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"Contents"=>stream,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Resources"=>dictionary! {"Font"=>dictionary! {"F1"=>font}}});
    doc.objects.insert(
        pages,
        Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>vec![page.into()],"Count"=>1}),
    );
    let root = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
    doc.trailer.set("Root", root);
    (doc, page, font)
}

#[test]
fn r8_ligatures_read_convert_replace_and_source_metrics() -> TestResult {
    let (mut doc, page, font) =
        fixture("BT /F1 12 Tf 1 0 0 1 50 700 Tm (ArmH A B C D E F G) Tj ET");
    let cmap = doc.add_object(Stream::new(dictionary! {}, b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def /CMapName /Test def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange 8 beginbfchar <41> <FB01> <42> <FB00> <43> <FB02> <44> <FB03> <45> <FB04> <46> <FB05> <47> <FB06> <48> <0105> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end".to_vec()));
    doc.get_dictionary_mut(font)?.set("ToUnicode", cmap);
    doc.get_dictionary_mut(font)?.set("FirstChar", 0);
    doc.get_dictionary_mut(font)?
        .set("Widths", vec![Object::Integer(500); 256]);
    let info = crate::fontdecode::FontInfo::load(doc.get_dictionary(font)?, &doc);
    let decoded = info.decode(b"A");
    assert_eq!(decoded.text, "fi");
    assert_eq!(decoded.glyph_text, "ﬁ");
    assert_eq!(decoded.chars, [(0, 1), (0, 1)]);
    let positioned = pdfedit::positioned_lines(&doc, page)?;
    // 18 source glyphs, not the expanded letter count.
    assert!(
        (positioned[0].width - 18.0 * 6.0).abs() < 0.001,
        "{positioned:?}"
    );
    let dir = std::env::temp_dir().join(format!("docs-r8-ligatures-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    doc.save(dir.join("in.pdf"))?;
    let opts = Options::new(dir.clone());
    let call = |value| -> Result<Output, Box<dyn std::error::Error>> {
        Ok(run(&serde_json::from_value(value)?, &opts)?)
    };
    let read = call(serde_json::json!({"op":"read","input":"in.pdf"}))?;
    let expected = "firmą fi ff fl ffi ffl st st";
    assert!(
        read.text.as_deref().is_some_and(|s| s.contains(expected)),
        "{:?}",
        read.text
    );
    call(serde_json::json!({"op":"convert","input":"in.pdf","output":"out.docx"}))?;
    let bytes = std::fs::read(dir.join("out.docx"))?;
    assert!(docx::read_text(&bytes)?.contains(expected));
    let xml = String::from_utf8(docxxml::entry(&bytes, "word/document.xml")?.ok_or("document")?)?;
    assert!(
        !xml.chars()
            .any(|ch| ('\u{fb00}'..='\u{fb06}').contains(&ch))
    );
    call(
        serde_json::json!({"op":"replace","input":"in.pdf","output":"edited.pdf","find":"firmą","replace":"company"}),
    )?;
    let edited = call(serde_json::json!({"op":"read","input":"edited.pdf"}))?;
    assert!(
        edited
            .text
            .as_deref()
            .is_some_and(|s| s.contains("company")),
        "{:?}",
        edited.text
    );
    assert!(!edited.text.as_deref().is_some_and(|s| s.contains("firmą")));
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn r8_pdfdocx_disables_legacy_width_balancing_not_run_spacing() -> TestResult {
    let (doc, page, _) = fixture("BT /F1 12 Tf 1 0 0 1 50 700 Tm (With company ) Tj ET");
    let result = pdfdocx::build(&doc, &[page])?;
    let settings =
        docxxml::parse(&docxxml::entry(&result.data, "word/settings.xml")?.ok_or("settings")?)?;
    assert!(
        settings
            .path(&["compat", "balanceSingleByteDoubleByteWidth"])
            .is_none()
    );
    let xml =
        String::from_utf8(docxxml::entry(&result.data, "word/document.xml")?.ok_or("document")?)?;
    assert!(!xml.contains("w:tab"));
    assert!(!xml.contains("w:ind"));
    assert!(!xml.contains("w:spacing w:val"));
    Ok(())
}

fn operators(doc: &Document, page: lopdf::ObjectId) -> Result<Vec<String>, Error> {
    Ok(pdfdocx::build(doc, &[page])?
        .warnings
        .into_iter()
        .filter(|w| w.code == MessageCode::PdfDocxBoundsApproximate)
        .map(|w| {
            assert_eq!(w.params.get("page"), Some(&serde_json::json!(1)));
            w.params
                .get("operator")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect())
}

#[test]
fn r8_every_approximate_paint_and_text_operator_is_named() -> TestResult {
    for op in ["f", "F", "f*", "S", "s", "B", "B*", "b", "b*"] {
        let (doc, page, _) = fixture(&format!("100 100 m 200 100 l {op}"));
        // A filled line is degenerate; strokes use hairline/dash below.
        if ["f", "F", "f*", "s", "b", "b*"].contains(&op) {
            assert_eq!(operators(&doc, page)?, [op], "{op}");
        }
        let (doc, page, _) = fixture(&format!("[3 2] 0 d 100 100 m 200 100 l {op}"));
        if !["f", "F", "f*"].contains(&op) {
            assert_eq!(operators(&doc, page)?, [op], "{op}");
        }
    }
    for (content, expected) in [
        ("BT /Missing 12 Tf 50 700 Td (Text) Tj ET", "Tj"),
        ("BT /Missing 12 Tf 50 700 Td [(Text)] TJ ET", "TJ"),
        ("BT /Missing 12 Tf 50 700 Td (Text) ' ET", "'"),
        ("BT /Missing 12 Tf 50 700 Td 0 0 (Text) \" ET", "\""),
        ("BI /W 1 /H 1 /BPC 8 /CS /RGB ID abc EI", "BI"),
        ("/Missing gs", "gs"),
        ("/Shade sh", "sh"),
        ("/Tile scn", "scn"),
        ("/Tile SCN", "SCN"),
        ("50 100 m 150 200 l 250 100 l h W n", "W"),
        ("50 100 m 150 200 l 250 100 l h W* n", "W*"),
        ("/Missing Do", "Do"),
        ("1 ExtensionPaint", "ExtensionPaint"),
    ] {
        let (doc, page, _) = fixture(content);
        assert_eq!(operators(&doc, page)?, [expected], "{content}");
    }
    Ok(())
}

#[test]
fn r8_clipped_text_and_all_render_modes_are_diagnosed() -> TestResult {
    for mode in 0..=7 {
        let (doc, page, _) = fixture(&format!("BT /F1 12 Tf {mode} Tr 50 700 Td (Text) Tj ET"));
        let expected: Vec<_> = if mode == 0 || mode == 3 {
            vec![]
        } else {
            vec!["Tr"]
        };
        assert_eq!(operators(&doc, page)?, expected, "mode {mode}");
    }
    for (op, show) in [
        ("Tj", "(Text) Tj"),
        ("TJ", "[(Text)] TJ"),
        ("'", "(Text) '"),
        ("\"", "0 0 (Text) \""),
    ] {
        let (doc, page, _) = fixture(&format!(
            "50 690 5 30 re W n BT /F1 12 Tf 50 700 Td {show} ET"
        ));
        assert_eq!(operators(&doc, page)?, [op], "{op}");
    }
    Ok(())
}

#[test]
fn r8_typical_documents_and_state_are_exact() -> TestResult {
    for content in [
        "BT /F1 12 Tf 50 700 Td (Ordinary) Tj 0 -20 Td [(Second) 10 ( line)] TJ (Third) ' 0 0 (Fourth) \" ET",
        "2 w 50 100 300 400 re S 50 200 m 350 200 l S 200 100 m 200 500 l S",
        "0 0 0 rg 50 100 300 400 re f",
        "q 50 100 300 400 re W* n 60 110 20 20 re f Q",
        "100 100 m W n 60 110 20 20 re f",
        "100 100 20 20 re n /DeviceRGB cs 0 0 0 scn 0 0 0 SCN",
    ] {
        let (doc, page, _) = fixture(content);
        assert!(operators(&doc, page)?.is_empty(), "{content}");
    }
    Ok(())
}

#[test]
fn r8_partial_ligatures_are_skipped_without_deleting_other_letters() -> TestResult {
    for all in [false, true] {
        let (mut doc, _, font) = fixture("BT /F1 12 Tf 50 700 Td (X) Tj ET");
        let cmap = doc.add_object(Stream::new(
            dictionary! {},
            b"1 beginbfchar <58> <FB03> endbfchar".to_vec(),
        ));
        doc.get_dictionary_mut(font)?.set("ToUnicode", cmap);
        let result = pdfedit::replace_in_document(
            &mut doc,
            "f",
            "A",
            all,
            &pdf::load_substitute_fonts(None),
        )?;
        assert_eq!(result.replaced, 0);
        assert_eq!(result.present, 2);
        assert_eq!(pdfedit::document_text(&doc).trim(), "ffi");
        if !all {
            let skipped = result
                .warnings
                .iter()
                .find(|w| w.code == MessageCode::PdfPartialGlyph)
                .ok_or("skipped")?;
            assert_eq!(skipped.params.get("skipped"), Some(&serde_json::json!(2)));
        }
        let result = pdfedit::replace_in_document(
            &mut doc,
            "ffi",
            "A",
            all,
            &pdf::load_substitute_fonts(None),
        )?;
        assert_eq!(result.replaced, 1);
        assert_eq!(pdfedit::document_text(&doc).trim(), "A");
    }
    Ok(())
}

#[test]
fn r8_partial_glyph_warning_uses_a_distinct_public_code() -> TestResult {
    let (mut doc, _, font) = fixture("BT /F1 12 Tf 50 700 Td (X) Tj ET");
    let cmap = doc.add_object(Stream::new(
        dictionary! {},
        b"1 beginbfchar <58> <FB03> endbfchar".to_vec(),
    ));
    doc.get_dictionary_mut(font)?.set("ToUnicode", cmap);
    let dir = std::env::temp_dir().join(format!("docs-r8-partial-glyph-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    doc.save(dir.join("in.pdf"))?;
    let opts = Options::new(dir.clone());
    for all in [false, true] {
        let output = run(
            &serde_json::from_value(
                serde_json::json!({"op":"replace","input":"in.pdf","output":"out.pdf","find":"f","replace":"A","all":all}),
            )?,
            &opts,
        )?;
        assert_eq!(
            output.summary_params.get("count"),
            Some(&serde_json::json!(0))
        );
        assert_eq!(output.warnings.len(), 1);
        assert_eq!(output.warnings[0].code, "pdf.partial_glyph");
        assert_eq!(
            output.warnings[0].params.get("skipped"),
            Some(&serde_json::json!(2))
        );
        assert!(!output.warnings[0].message.contains('{'));
        assert_eq!(
            pdf::extract_text(&dir.join("out.pdf"), "out.pdf")?.trim(),
            "ffi"
        );
    }
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn r8_complex_strokes_fills_and_clipping_name_each_paint_operator() -> TestResult {
    for op in ["S", "s", "B", "B*", "b", "b*"] {
        for path in [
            "0 w 100 100 m 200 100 l",
            "100 100 m 100 200 200 200 200 100 c",
            "100 100 m 200 200 200 100 v",
            "100 100 m 100 200 200 100 y",
            "2 j 100 100 100 100 re",
            "0 0 0 0 0 0 cm 100 100 m 200 100 l",
            "100 100 m 100 100 l",
        ] {
            let (doc, page, _) = fixture(&format!("{path} {op}"));
            assert_eq!(operators(&doc, page)?, [op], "{path} {op}");
        }
    }
    for op in ["f", "F", "f*"] {
        for path in [
            "100 100 50 50 re 200 100 50 50 re",
            "100 100 10 10 re W n 90 90 m 130 90 l 130 130 l 90 130 l h",
        ] {
            let (doc, page, _) = fixture(&format!("{path} {op}"));
            assert_eq!(operators(&doc, page)?, [op], "{path} {op}");
        }
    }
    Ok(())
}

#[test]
fn r8_transformed_clip_rectangles_name_the_clip_operator() -> TestResult {
    for op in ["W", "W*"] {
        let (doc, page, _) = fixture(&format!("1 0.2 0 1 0 0 cm 50 100 300 400 re {op} n"));
        assert_eq!(operators(&doc, page)?, [op]);
    }
    Ok(())
}

#[test]
fn r8_unreadable_missing_metrics_and_rotated_shows_are_named() -> TestResult {
    for (op, show) in [
        ("Tj", "(Text) Tj"),
        ("TJ", "[(Text)] TJ"),
        ("'", "(Text) '"),
        ("\"", "0 0 (Text) \""),
    ] {
        for kind in [
            "missing metrics",
            "missing outlines",
            "unreadable",
            "rotated",
        ] {
            let tm = if kind == "rotated" {
                "0.8 0.6 -0.6 0.8 100 600 Tm"
            } else {
                "50 700 Td"
            };
            let (mut doc, page, font) = fixture(&format!("BT /F1 12 Tf {tm} {show} ET"));
            let font = doc.get_dictionary_mut(font)?;
            if kind == "unreadable" {
                font.set("Subtype", "Type0");
                font.set("Encoding", "Identity-H");
            } else if kind != "rotated" {
                font.set("BaseFont", "UnavailableFace");
                if kind == "missing outlines" {
                    font.set("FirstChar", 0);
                    font.set("Widths", vec![Object::Integer(500); 256]);
                }
            }
            assert_eq!(operators(&doc, page)?, [op], "{kind}: {op}");
        }
    }
    Ok(())
}

fn xobject(
    doc: &mut Document,
    page: lopdf::ObjectId,
    stream: Stream,
) -> Result<lopdf::ObjectId, Error> {
    let id = doc.add_object(stream);
    doc.get_dictionary_mut(page)?
        .get_mut(b"Resources")?
        .as_dict_mut()?
        .set("XObject", dictionary! {"X"=>id});
    Ok(id)
}

#[test]
fn r8_images_masks_and_oblique_clipping_are_diagnosed() -> TestResult {
    for kind in ["plain", "SMask", "Mask", "ImageMask", "oblique clip"] {
        let content = if kind == "oblique clip" {
            "110 110 10 10 re W n 80 60 -60 80 100 100 cm /X Do"
        } else {
            "100 0 0 100 100 100 cm /X Do"
        };
        let (mut doc, page, _) = fixture(content);
        let mut image = Stream::new(
            dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>1,"Height"=>1,"BitsPerComponent"=>8,"ColorSpace"=>"DeviceRGB"},
            vec![0, 0, 0],
        );
        match kind {
            "SMask" => {
                let mask = doc.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>1,"Height"=>1,"BitsPerComponent"=>8,"ColorSpace"=>"DeviceGray"}, vec![128]));
                image.dict.set("SMask", mask);
            }
            "Mask" => image.dict.set(
                "Mask",
                vec![0.into(), 0.into(), 0.into(), 0.into(), 0.into(), 0.into()],
            ),
            "ImageMask" => image.dict.set("ImageMask", true),
            _ => {}
        }
        xobject(&mut doc, page, image)?;
        let expected: Vec<_> = if kind == "plain" { vec![] } else { vec!["Do"] };
        assert_eq!(operators(&doc, page)?, expected, "{kind}");
    }
    Ok(())
}

#[test]
fn r8_form_limits_cycles_groups_geometry_and_inherited_state_are_named() -> TestResult {
    for kind in [
        "plain",
        "Group",
        "no bbox",
        "oblique",
        "cycle",
        "depth",
        "unknown subtype",
        "text state",
    ] {
        let (mut doc, page, _) = fixture("2 Tc /X Do");
        let mut form = Stream::new(
            dictionary! {"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),600.into(),800.into()]},
            b"100 100 20 20 re f".to_vec(),
        );
        match kind {
            "Group" => form.dict.set("Group", dictionary! {"S"=>"Transparency"}),
            "no bbox" => {
                form.dict.remove(b"BBox");
            }
            "oblique" => form.dict.set(
                "Matrix",
                vec![1.into(), 0.2.into(), 0.into(), 1.into(), 0.into(), 0.into()],
            ),
            "unknown subtype" => form.dict.set("Subtype", "Other"),
            "text state" => {
                form.set_content(b"BT /F1 12 Tf 50 700 Td (Inherited spacing) Tj ET".to_vec())
            }
            _ => {}
        }
        let mut id = xobject(&mut doc, page, form)?;
        if kind == "cycle" {
            doc.get_object_mut(id)?
                .as_stream_mut()?
                .set_content(b"/X Do".to_vec());
        } else if kind == "depth" {
            for _ in 0..33 {
                id = doc.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Form","BBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Resources"=>dictionary! {"XObject"=>dictionary! {"X"=>id}}}, b"/X Do".to_vec()));
            }
            doc.get_dictionary_mut(page)?
                .get_mut(b"Resources")?
                .as_dict_mut()?
                .set("XObject", dictionary! {"X"=>id});
        }
        let expected: Vec<_> = if kind == "plain" { vec![] } else { vec!["Do"] };
        assert_eq!(operators(&doc, page)?, expected, "{kind}");
    }
    Ok(())
}

#[test]
fn r8_extgstate_supported_values_and_each_unsupported_key() -> TestResult {
    for (key, value) in [
        ("ca", Object::Real(0.5)),
        ("CA", Object::Real(0.5)),
        ("BM", Object::Name(b"Multiply".to_vec())),
        ("SMask", Object::Dictionary(dictionary! {"S"=>"Luminosity"})),
        (
            "D",
            Object::Array(vec![vec![3.into(), 2.into()].into(), 0.into()]),
        ),
        ("Font", Object::Array(vec![])),
    ] {
        let (mut doc, page, _) = fixture("/State gs /State gs 100 100 20 20 re f");
        let mut state = dictionary! {"Type"=>"ExtGState","LW"=>2,"LC"=>1,"LJ"=>0,"ca"=>1,"CA"=>1,"BM"=>"Normal","SMask"=>"None"};
        doc.get_dictionary_mut(page)?
            .get_mut(b"Resources")?
            .as_dict_mut()?
            .set("ExtGState", dictionary! {"State"=>state.clone()});
        assert!(operators(&doc, page)?.is_empty(), "control {key}");
        state.set(key, value);
        doc.get_dictionary_mut(page)?
            .get_mut(b"Resources")?
            .as_dict_mut()?
            .set("ExtGState", dictionary! {"State"=>state});
        assert_eq!(operators(&doc, page)?, ["gs"], "{key}");
    }
    Ok(())
}

#[test]
fn r8_saved_text_state_differences_are_diagnosed_at_use() -> TestResult {
    for state in [
        "5 Tc",
        "5 Tw",
        "80 Tz",
        "4 Ts",
        "3 Tr",
        "30 TL",
        "/F1 24 Tf",
    ] {
        let (doc, page, _) = fixture(&format!(
            "BT /F1 12 Tf 50 700 Td q {state} Q T* (Text) Tj ET"
        ));
        assert_eq!(operators(&doc, page)?, ["Q"], "{state}");
    }
    Ok(())
}

#[test]
fn r8_page_geometry_warnings_keep_page_numbers_and_deduplicate_per_page() -> TestResult {
    for (key, ordinary, unusual) in [
        ("Rotate", Object::Integer(360), Object::Integer(90)),
        ("UserUnit", Object::Integer(1), Object::Integer(2)),
        (
            "CropBox",
            vec![0.into(), 0.into(), 600.into(), 800.into()].into(),
            vec![0.into(), 0.into(), 300.into(), 400.into()].into(),
        ),
        ("Annots", vec![].into(), vec![Object::Null].into()),
    ] {
        let (mut doc, page, _) = fixture("100 100 20 20 re f");
        let parent = doc.get_dictionary(page)?.get(b"Parent")?.as_reference()?;
        doc.get_dictionary_mut(parent)?.set(key, ordinary);
        assert!(operators(&doc, page)?.is_empty(), "ordinary {key}");
        let second = doc.add_object(doc.get_dictionary(page)?.clone());
        doc.get_dictionary_mut(second)?.set(key, unusual);
        let result = pdfdocx::build(&doc, &[page, second])?;
        let warnings: Vec<_> = result
            .warnings
            .iter()
            .filter(|w| w.code == MessageCode::PdfDocxBoundsApproximate)
            .collect();
        assert_eq!(warnings.len(), 1, "{key}");
        assert_eq!(warnings[0].params.get("page"), Some(&serde_json::json!(2)));
        assert_eq!(
            warnings[0].params.get("operator"),
            Some(&serde_json::json!(key))
        );
    }
    Ok(())
}

fn html_lines(html: &str) -> Result<Vec<pdfedit::PositionedLine>, Error> {
    let bytes = htmlpdf::render(html, &pdf::load_font(None)?, &Default::default())?;
    let doc = Document::load_mem(&bytes)?;
    let mut lines = Vec::new();
    for page in doc.get_pages().into_values() {
        lines.extend(pdfedit::positioned_lines(&doc, page)?);
    }
    Ok(lines)
}

#[test]
fn r8_unstyled_html_paragraph_list_and_table_baselines_match_chromium() -> TestResult {
    let lines = html_lines(
        "<p>First</p><p>Second</p><ul><li>Bullet one</li><li>Bullet two</li></ul><table><tr><td>Cell one</td></tr><tr><td>Cell two</td></tr></table>",
    )?;
    let y = |text| {
        lines
            .iter()
            .find(|l| l.text.trim() == text)
            .map(|l| l.y)
            .ok_or("line")
    };
    // Chromium: 18px normal line box + 16px collapsed paragraph margins;
    // lists use the same line box, table rows add 2px padding + 2px spacing.
    for (first, second, gap) in [
        ("First", "Second", 25.5),
        ("• Bullet one", "• Bullet two", 13.5),
        ("Cell one", "Cell two", 16.5),
    ] {
        assert!(
            (y(first)? - y(second)? - gap).abs() < 0.03,
            "{first}: {lines:?}"
        );
    }
    assert!(lines.iter().all(|l| (l.size - 12.0).abs() < 0.03));
    Ok(())
}

#[test]
fn r8_author_line_height_and_font_are_not_overridden() -> TestResult {
    for html in [
        "<html><head><style>body {font-family:'Noto Sans';font-size:20pt;line-height:2;} p {margin:0}</style></head><body><p>First</p><p>Second</p></body></html>",
        "<div style = \"font-family:'Noto Sans';font-size:20pt;line-height:2\"><p style='margin:0'>First</p><p style='margin:0'>Second</p></div>",
    ] {
        let lines = html_lines(html)?;
        assert_eq!(lines.len(), 2);
        assert!((lines[0].size - 20.0).abs() < 0.03);
        assert!((lines[0].y - lines[1].y - 40.0).abs() < 0.03, "{lines:?}");
    }
    Ok(())
}
