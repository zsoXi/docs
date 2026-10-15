//! Absolute Chromium print coordinates, in points from the top-left of the sheet.
//! Recorded from Chromium's PDF text matrices (CSS px × 0.75), not native output.
use super::*;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn check(html: &str, reference: &[(&str, f64, f64)]) -> Result {
    let (prepared, anchor) =
        markers::prepare(&with_defaults(html), markers::has_author_list_style(html));
    let mut native = pdf::html_document(
        &prepared,
        &pdf::load_font(None)?,
        &Default::default(),
        &page_setup(html),
        &[],
        &mut Vec::new(),
    )?;
    markers::paint(&mut native, &anchor);
    let bytes = native.save(&PdfSaveOptions::default(), &mut Vec::new());
    let mut glyphs = Vec::new();
    let mut position = (0.0, 0.0);
    for op in &native.pages[0].ops {
        match op {
            printpdf::Op::SetTextMatrix {
                matrix: printpdf::TextMatrix::Raw(m),
            } => position = (f64::from(m[4]), f64::from(m[5])),
            printpdf::Op::ShowText { items } => {
                for item in items {
                    if let printpdf::TextItem::GlyphIds(gs) = item {
                        let text: String = gs.iter().filter_map(|g| g.cid.as_deref()).collect();
                        glyphs.push((text, position));
                    }
                }
            }
            _ => {}
        }
    }
    let doc = lopdf::Document::load_mem(&bytes)?;
    let page = *doc.get_pages().values().next().ok_or("page")?;
    let bounds = crate::pdfedit::page_bounds(&doc, page)?;
    let lines = crate::pdfedit::positioned_lines(&doc, page)?;
    for (label, x, baseline) in reference {
        let line = lines
            .iter()
            .find(|l| {
                l.text == *label
                    || l.text
                        .strip_prefix(['•', '◦', '▪'])
                        .is_some_and(|s| s.trim() == *label)
            })
            .ok_or_else(|| format!("missing {label}: {lines:?}"))?;
        // Extractor lines include invisible list labels; measure the visible text origin.
        let initial = label.chars().next().ok_or("label")?;
        let actual_x = glyphs
            .iter()
            .filter(|(s, (_, y))| s.starts_with(initial) && (y - line.y).abs() < 0.01)
            .map(|(_, (x, _))| *x)
            .min_by(f64::total_cmp)
            .ok_or("glyph origin")?;
        assert!((actual_x - x).abs() <= 1.0, "{label} x: {actual_x} vs {x}");
        let top = bounds.media[3] - line.y;
        assert!(
            (top - baseline).abs() <= 1.0,
            "{label} baseline: {top} vs {baseline}"
        );
    }
    Ok(())
}

#[test]
fn chromium_absolute_large_list_start_and_nested_baselines() -> Result {
    check(
        "<html><head><style>body{font-family:'Liberation Serif';font-size:24pt;line-height:36px}</style></head><body><ul><li>Large disc<ul><li>Large circle<ul><li>Large square</li></ul></li></ul></li></ul></body></html>",
        &[
            ("Large disc", 63.75, 55.5),
            ("Large circle", 93.75, 82.5),
            ("Large square", 123.75, 109.5),
        ],
    )
}

#[test]
fn chromium_absolute_table_rows_and_following_paragraph() -> Result {
    // The same local reference used for visual inspection: A4, 20mm, Noto Sans.
    check(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><style>
@page { size: A4; margin: 20mm; }
body { font-family: "Noto Sans"; font-size: 11pt; margin: 0; }
h1 { font-size: 20pt; font-weight: bold; margin: 0 0 12pt; }
p { margin: 0 0 6pt; }
table { border-collapse: collapse; width: 330pt; margin-top: 12pt; }
td, th { border: 1px solid #333; padding: 4pt; text-align: left; }
th { font-weight: bold; background: #e8edf2; }
</style></head><body>
<h1>Umowa serwisowa nr 2026/10</h1>
<p>Zawarta pomiędzy firmą <b>Kowalski Sp. z o.o.</b> a klientem Zażółć Gęślą.</p>
<p>Opłata miesięczna wynosi <b>1500 zł netto</b>. Okres umowy: 12 miesięcy.</p>
<table><tr><th>Usługa</th><th>Warunki</th></tr><tr><td>Serwis podstawowy</td><td>Reakcja do 24 godzin</td></tr><tr><td>Przegląd okresowy</td><td>Raz na kwartał</td></tr></table>
<p>Strony akceptują warunki umowy i terminy płatności.</p>
</body></html>"#,
        &[
            ("Usługa", 61.7461, 160.5),
            ("Warunki", 220.8516, 160.5),
            ("Serwis podstawowy", 61.7461, 184.5),
            ("Przegląd okresowy", 61.7461, 208.5),
            (
                "Strony akceptują warunki umowy i terminy płatności.",
                57.0,
                228.0,
            ),
        ],
    )
}
