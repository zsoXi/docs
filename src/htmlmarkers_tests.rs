//! List geometry measured from Chromium's local PDF output (CSS px = 0.75pt).
use super::*;
use crate::{Error, pdf, pdfedit};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn layout(html: &str) -> Result<PdfDocument, Error> {
    let (prepared, anchor) = prepare(
        &super::super::with_defaults(html),
        has_author_list_style(html),
    );
    let mut document = pdf::html_document(
        &prepared,
        &pdf::load_font(None)?,
        &Default::default(),
        &super::super::page_setup(html),
        &[],
        &mut Vec::new(),
    )?;
    paint(&mut document, &anchor);
    Ok(document)
}

fn lines(document: &PdfDocument) -> Result<Vec<pdfedit::PositionedLine>, Error> {
    let bytes = document.save(&printpdf::PdfSaveOptions::default(), &mut Vec::new());
    let doc = lopdf::Document::load_mem(&bytes)?;
    let mut lines = Vec::new();
    for page in doc.get_pages().into_values() {
        lines.extend(pdfedit::positioned_lines(&doc, page)?);
    }
    Ok(lines)
}

fn glyphs(doc: &PdfDocument) -> Vec<(String, f32, f32)> {
    let mut out = Vec::new();
    let mut pos = (0.0, 0.0);
    for page in &doc.pages {
        for op in &page.ops {
            match op {
                Op::SetTextMatrix {
                    matrix: TextMatrix::Raw(m),
                } => pos = (m[4], m[5]),
                Op::ShowText { items } => {
                    for item in items {
                        if let TextItem::GlyphIds(gs) = item {
                            out.push((
                                gs.iter().filter_map(|g| g.cid.as_deref()).collect(),
                                pos.0,
                                pos.1,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn near(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.02,
        "actual={actual}, expected={expected}"
    );
}

#[test]
fn browser_default_disc_circle_square_geometry_and_nested_text() -> TestResult {
    let doc = layout(
        "<p>Before</p><ul><li>Outer<ul><li>Middle<ul><li>Inner</li></ul></li></ul></li></ul><p>After</p>",
    )?;
    let gs = glyphs(&doc);
    let positions: Vec<_> = ["B", "O", "M", "I"]
        .into_iter()
        .map(|s| gs.iter().find(|g| g.0 == s).ok_or("glyph"))
        .collect::<Result<_, _>>()?;
    let shapes: Vec<_> = doc
        .pages
        .iter()
        .flat_map(|p| &p.ops)
        .filter_map(|op| match op {
            Op::DrawPolygon { polygon } if polygon.mode != PaintMode::Clip => Some(polygon),
            _ => None,
        })
        .collect();
    assert_eq!(shapes.len(), 3);
    for (i, (shape, origin)) in shapes.iter().zip(&positions[1..]).enumerate() {
        near(origin.1 - positions[0].1, 30.0 * (i + 1) as f32);
        near(positions[0].2 - origin.2, 25.5 + 13.5 * i as f32);
        let points = &shape.rings[0].points;
        let min_x = points.iter().map(|p| p.p.x.0).fold(f32::INFINITY, f32::min);
        let max_x = points
            .iter()
            .map(|p| p.p.x.0)
            .fold(f32::NEG_INFINITY, f32::max);
        let min_y = points.iter().map(|p| p.p.y.0).fold(f32::INFINITY, f32::min);
        let max_y = points
            .iter()
            .map(|p| p.p.y.0)
            .fold(f32::NEG_INFINITY, f32::max);
        // Chromium: left edge -16px, size 5px, top baseline-7px.
        near(min_x - origin.1, -12.0);
        near(max_x - min_x, 3.75);
        near(max_y - min_y, 3.75);
        near(max_y - origin.2, 5.25);
        assert_eq!(
            shape.mode,
            if i == 1 {
                PaintMode::Stroke
            } else {
                PaintMode::Fill
            }
        );
        assert_eq!(points.len(), if i == 2 { 4 } else { 13 });
    }
    assert!(
        doc.pages[0]
            .ops
            .iter()
            .any(|o| matches!(o, Op::SetOutlineThickness { pt } if (pt.0 - 0.75).abs() < 0.001))
    );
    let text: String = lines(&doc)?.iter().map(|l| l.text.as_str()).collect();
    for word in ["Outer", "Middle", "Inner"] {
        assert_eq!(text.matches(word).count(), 1, "{text}");
    }
    assert!(!text.contains('\u{200b}'), "{text}");
    Ok(())
}

#[test]
fn browser_large_markers_keep_symbol_box_and_hanging_gap() -> TestResult {
    let doc = layout(
        "<html><head><style>body{font-family:'Liberation Serif';font-size:24pt;line-height:36px}</style></head><body><ul><li>Large</li></ul></body></html>",
    )?;
    let gs = glyphs(&doc);
    let origin = gs.iter().find(|g| g.0 == "L").ok_or("glyph")?;
    let polygon = doc.pages[0]
        .ops
        .iter()
        .find_map(|op| match op {
            Op::DrawPolygon { polygon } if polygon.mode == PaintMode::Fill => Some(polygon),
            _ => None,
        })
        .ok_or("polygon")?;
    let points = &polygon.rings[0].points;
    let min = points.iter().map(|p| p.p.x.0).fold(f32::INFINITY, f32::min);
    let max = points
        .iter()
        .map(|p| p.p.x.0)
        .fold(f32::NEG_INFINITY, f32::max);
    near(min - origin.1, -19.5); // Chromium: -26px at a 32px font.
    near(max - min, 7.5); // 10 CSS pixels.
    near(
        points
            .iter()
            .map(|p| p.p.y.0)
            .fold(f32::NEG_INFINITY, f32::max)
            - origin.2,
        10.5,
    );
    Ok(())
}

#[test]
fn browser_ordered_start_alpha_roman_reversed_and_value() -> TestResult {
    for (html, expected, hanging) in [
        (
            "<ol start='9'><li>Item</li><li>Next</li></ol>",
            ["9. Item", "10. Next"],
            12.0,
        ),
        (
            "<ol type='a' start='26'><li>Item</li><li>Next</li></ol>",
            ["z. Item", "aa. Next"],
            11.33,
        ),
        (
            "<ol style='list-style-type:lower-alpha' start='26'><li>Item</li><li>Next</li></ol>",
            ["z. Item", "aa. Next"],
            11.33,
        ),
        (
            "<ol style='list-style-type:lower-roman' start='8'><li>Item</li><li>Next</li></ol>",
            ["viii. Item", "ix. Next"],
            22.01,
        ),
        (
            "<ol reversed><li>Item</li><li>Next</li></ol>",
            ["2. Item", "1. Next"],
            12.0,
        ),
        (
            "<ol reversed start='5'><li>Item</li><li value='2'>Next</li></ol>",
            ["5. Item", "2. Next"],
            12.0,
        ),
        (
            "<ol reversed='false' start='0'><li>Item</li><li>Next</li></ol>",
            ["0. Item", "-1. Next"],
            12.0,
        ),
    ] {
        let doc = layout(html)?;
        let ls = lines(&doc)?;
        let text: Vec<_> = ls.iter().map(|l| l.text.trim()).collect();
        assert_eq!(text, expected, "{html}: {ls:?}");
        let gs = glyphs(&doc);
        let body = gs.iter().find(|g| g.0 == "I").ok_or("body glyph")?;
        near(body.1 - gs[0].1, hanging);
        near(body.2 - gs[0].2, 0.0);
        assert!(!text.join("").contains('\u{200b}'));
    }
    Ok(())
}

#[test]
fn browser_nested_reversed_counts_direct_items_not_descendants() -> TestResult {
    let doc = layout(
        "<ol reversed><li>First<ol start='7'><li>Child</li><li>Second</li></ol></li><li>Last</li></ol>",
    )?;
    let text: Vec<_> = lines(&doc)?
        .into_iter()
        .map(|l| l.text.trim().to_string())
        .collect();
    assert_eq!(text, ["2. First", "7. Child", "8. Second", "1. Last"]);
    Ok(())
}

#[test]
fn browser_literal_bullets_and_zero_width_text_are_not_markers() -> TestResult {
    let html = "<p>Literal <span>• </span><span>◦ </span>▪ end</p><p>&#x0200b;&#0008203;Original</p><ul><li>&#8203;Item</li></ul>";
    let doc = layout(html)?;
    let shapes = doc.pages[0]
        .ops
        .iter()
        .filter(|op| matches!(op, Op::DrawPolygon { polygon } if polygon.mode != PaintMode::Clip))
        .count();
    assert_eq!(shapes, 1); // Only the generated list marker is a vector.
    let gs = glyphs(&doc);
    assert_eq!(gs.iter().filter(|g| g.0 == "•").count(), 2);
    assert_eq!(gs.iter().filter(|g| g.0 == "◦").count(), 1);
    assert_eq!(gs.iter().filter(|g| g.0 == "▪").count(), 1);
    assert_eq!(gs.iter().filter(|g| g.0 == "\u{200b}").count(), 3);
    Ok(())
}

#[test]
fn browser_author_list_style_wins_over_nested_defaults_and_type_hint() -> TestResult {
    let doc = layout(
        "<html><head><style>ul{list-style-type:square}ol{list-style-type:lower-roman}</style></head><body><ul><li>Outer<ul><li>Inner</li></ul></li></ul><ol type='a' start='8'><li>Roman</li></ol></body></html>",
    )?;
    let text: String = lines(&doc)?.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(text.matches('▪').count(), 2, "{text}");
    assert!(!text.contains('◦'), "{text}");
    assert!(text.contains("viii. Roman"), "{text}");
    let doc = layout("<ol type='a' style='color:red'><li>Alpha</li></ol>")?;
    assert!(lines(&doc)?.iter().any(|l| l.text.trim() == "a. Alpha"));
    Ok(())
}

#[test]
fn browser_preparation_preserves_raw_text_quotes_and_integer_limits() {
    let html = "<!-- <ol reversed> --><script>const x = '<li>fake</li>';</script><style>li:before{content:'<li>'}</style><OL START='2147483647' REVERSED title='a > b'><LI style='color:red'>One</LI><LI VALUE='-2147483648'>Two</LI></OL>";
    let (prepared, anchor) = prepare(html, false);
    assert_eq!(prepared.matches(&anchor).count(), 2);
    assert!(prepared.contains("const x = '<li>fake</li>'"));
    assert!(prepared.contains("title='a > b'"));
    assert!(prepared.contains("counter-reset:list-item 2147483646"));
    assert!(prepared.contains("counter-reset:list-item -2147483648"));
    assert!(prepared.contains("color:red;counter-reset:"));
    for malformed in [
        "<ol",
        "<ol reversed",
        "<ol reversed='",
        "<li title='>'",
        "<ol><li = x>",
        "<ol><li>Unclosed",
    ] {
        let _ = prepare(malformed, false); // No byte slicing panic on untrusted HTML.
    }
}

#[test]
fn browser_markers_survive_wrapping_styles_and_multiple_pages() -> TestResult {
    let mut html = String::from("<ol start='90'>");
    for i in 0..80 {
        html.push_str(&format!(
            "<li><b>Row{i}</b> with <i>mixed</i> formatting</li>"
        ));
    }
    html.push_str("</ol>");
    let doc = layout(&html)?;
    assert!(doc.pages.len() > 1);
    let text: String = lines(&doc)?.iter().map(|l| l.text.as_str()).collect();
    for i in 0..80 {
        assert_eq!(text.matches(&format!("Row{i} with")).count(), 1, "row {i}");
    }
    assert!(text.contains("90. ") && text.contains("169. "), "{text}");
    assert!(!text.contains('\u{200b}'));
    Ok(())
}
