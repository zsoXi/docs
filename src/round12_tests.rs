//! PDF→DOCX regressions measured against an independent Writer renderer.
use super::*;
use crate::read::raster::{Scratch, decode};
use lopdf::Document;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const REPORT: &str = "# Quarterly Performance Report\n\n**Q3 2026 | Northstar Analytics Ltd**\n\nRevenue grew by **18%** while operating costs fell by *7%*. All values are in USD millions.\n\n| Region | Q2 | Q3 | Change |\n|---|---:|---:|---:|\n| North | 1.9 | 2.3 | 21% |\n| South | 1.4 | 1.6 | 14% |\n| West | 1.1 | 1.3 | 18% |\n\n## Revenue trend\n\n<div style=\"height:190pt\"></div>\n\nFigure 1. Quarterly revenue trend, Q1 to Q3.\n\n1. Expand the enterprise sales team.\n2. Continue cost controls.\n   - Review software renewals.\n   - Consolidate hosting contracts.\n\n> Forecasts depend on customer retention and market conditions.\n\n```text\nRevenue = subscriptions + services\nMargin = profit / revenue\n```\n\n[Reporting methodology](https://example.org/methodology)\n";
const CONTRACT: &str = "# Professional Services Agreement\n\n**Northstar Analytics Ltd** and **Cedar Consulting Ltd** agree to the following terms.\n\n## Scope of services\n\nThe consultant will deliver monthly analysis and a quarterly review.\n\n| Deliverable | Due date |\n|---|---|\n| Analysis | Monthly |\n| Review | Quarterly |\n| Summary | Annually |\n\n## Fees and payment\n\nThe fee is **1,500.00 USD** payable within thirty days.\n\n1. Submit the monthly report.\n2. Obtain written approval.\n\n*Signed by both parties.*\n";
const INVOICE: &str = "# Invoice INV-2026-031\n\n**Cedar Consulting Ltd**\n\nBill to: Northstar Analytics Ltd\n\n| Description | Quantity | Amount |\n|---|---:|---:|\n| Analysis | 10 | 1,000.00 |\n| Review | 2 | 500.00 |\n| Total | 12 | 1,500.00 |\n\n## Payment details\n\nPayment due: 30 September 2026.\n\nReference: INV-2026-031\n\n*Thank you for your business.*\n";

fn source(markdown: &str, chart: bool) -> Result<Document> {
    let html = markdown::full_html(&markdown::to_html(markdown));
    let mut pdf = Document::load_mem(&pdf::html_to_pdf(&html, &pdf::load_font(None)?)?)?;
    if chart {
        let page = pdf.get_pages().into_values().next().ok_or("page")?;
        let lines = pdfedit::positioned_lines(&pdf, page)?;
        let caption = lines
            .iter()
            .find(|l| l.text.starts_with("Figure 1."))
            .ok_or("caption")?;
        let y = caption.y + 28.0;
        let x = caption.x + 30.0;
        // Actual filled/stroked PDF paths, not a raster image fixture.
        let link = lines
            .iter()
            .find(|l| l.text.contains("Reporting methodology"))
            .ok_or("link source")?;
        let annotation = pdf.add_object(lopdf::dictionary! {
            "Type" => "Annot", "Subtype" => "Link", "Border" => vec![0.into(), 0.into(), 0.into()],
            "Rect" => vec![link.x.into(), (link.y - 3.0).into(), (link.x + link.width).into(), (link.y + link.size).into()],
            "A" => lopdf::dictionary! {"S" => "URI", "URI" => lopdf::Object::string_literal("https://example.org/methodology")}
        });
        pdf.get_dictionary_mut(page)?
            .set("Annots", vec![lopdf::Object::Reference(annotation)]);
        pdf.add_page_contents(
            page,
            format!(
                "q .025 .27 .68 RG .5 w {} {} m {} {} l S Q",
                link.x,
                link.y - 1.5,
                link.x + link.width,
                link.y - 1.5
            )
            .into_bytes(),
        )?;
        pdf.add_page_contents(page, format!("q .16 .36 .57 rg {} {} 75 84 re f {} {} 75 110 re f {} {} 75 136 re f .5 G .5 w {} {} m {} {} l S Q", x, y, x+120.0, y, x+240.0, y, x-20.0, y, x+345.0, y).into_bytes())?;
    }
    Ok(pdf)
}

fn xml(bytes: &[u8], name: &str) -> Result<docxxml::Element> {
    Ok(docxxml::parse(
        &docxxml::entry(bytes, name)?.ok_or("missing XML part")?,
    )?)
}
fn text(node: &docxxml::Element) -> String {
    let mut out = node.text.clone();
    for c in &node.children {
        out.push_str(&text(c));
    }
    out
}
fn positions(pdf: &Document, key: &str) -> Result<(f64, f64)> {
    for page in pdf.get_pages().into_values() {
        for line in pdfedit::positioned_lines(pdf, page)? {
            if line.text.contains(key) {
                return Ok((line.x, line.y));
            }
        }
    }
    Err(format!("missing key: {key}").into())
}

#[test]
fn r12_report_contract_invoice_roundtrip_geometry_and_structure() -> Result {
    for (name, markdown, chart, keys) in [
        (
            "report",
            REPORT,
            true,
            vec![
                "Quarterly Performance Report",
                "Revenue trend",
                "Figure 1.",
                "Expand the enterprise",
                "Review software",
                "Revenue =",
                "Reporting methodology",
            ],
        ),
        (
            "contract",
            CONTRACT,
            false,
            vec![
                "Professional Services Agreement",
                "Scope of services",
                "Fees and payment",
                "Signed by both",
            ],
        ),
        (
            "invoice",
            INVOICE,
            false,
            vec!["Invoice INV", "Payment details", "Payment due", "Thank you"],
        ),
    ] {
        let dir = Scratch::new()?;
        let mut original = source(markdown, chart)?;
        original.save(dir.0.join("source.pdf"))?;
        let opts = Options::new(dir.0.clone());
        let result = run(
            &serde_json::from_value(
                serde_json::json!({"op":"convert","input":"source.pdf","output":"converted.docx"}),
            )?,
            &opts,
        )?;
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.code == "convert.layout_approximate"),
            "{:?}",
            result.warnings
        );
        assert!(
            !result.warnings.iter().any(|w| matches!(
                w.code.as_str(),
                "pdfdocx.font_substituted"
                    | "pdfdocx.vector_geometry"
                    | "pdfdocx.link_extent"
                    | "pdfdocx.vector_text_rasterized"
            )),
            "{:?}",
            result.warnings
        );
        let bytes = std::fs::read(dir.0.join("converted.docx"))?;
        let root = xml(&bytes, "word/document.xml")?;
        let body = root.child("body").ok_or("body")?;
        let plain = text(body);
        let mut offset = 0;
        for key in &keys {
            let at = plain[offset..]
                .find(*key)
                .ok_or_else(|| format!("{name}: block out of order: {key}"))?;
            offset += at + key.len();
        }
        let tables: Vec<_> = body.children.iter().filter(|n| n.name == "tbl").collect();
        assert_eq!(tables.len(), 1, "{name}: {root:?}");
        assert_eq!(
            tables[0].children.iter().filter(|n| n.name == "tr").count(),
            4,
            "{name}: {root:?}"
        );
        if chart {
            let content =
                String::from_utf8(docxxml::entry(&bytes, "word/document.xml")?.ok_or("document")?)?;
            let table_end = content.find("</w:tbl>").ok_or("table end")?;
            let heading = content.find("Revenue trend").ok_or("heading")?;
            let picture = content.find("<w:drawing>").ok_or("chart image")?;
            let caption = content.find("Figure 1.").ok_or("caption")?;
            let item = content.find("Expand the enterprise").ok_or("list item")?;
            let code = content.find("Revenue =").ok_or("code")?;
            let link = content.find("<w:hyperlink").ok_or("link")?;
            assert!(
                table_end < heading
                    && heading < picture
                    && picture < caption
                    && caption < item
                    && item < code
                    && code < link
            );
            assert!(!text(tables[0]).contains("Revenue trend"));
            assert!(content.contains("Noto Sans Mono"));
            assert!(content.contains("w:fill=\"F2F2F2\""), "{content}");
            assert!(content.contains("<w:u w:val=\"single\""));
            let rels = xml(&bytes, "word/_rels/document.xml.rels")?;
            assert!(
                rels.children
                    .iter()
                    .any(|r| r.attr("Target") == Some("https://example.org/methodology"))
            );
            let media = rels
                .children
                .iter()
                .find(|r| r.attr("Type").is_some_and(|t| t.ends_with("/image")))
                .and_then(|r| r.attr("Target"))
                .ok_or("media")?;
            let png = docxxml::entry(&bytes, &format!("word/{media}"))?.ok_or("image data")?;
            let image = image::load_from_memory(&png)?;
            // A 365pt chart must have at least 200 pixels per inch.
            assert!(f64::from(image.width()) >= 365.0 / 72.0 * 200.0);
            assert!(
                image
                    .to_rgb8()
                    .pixels()
                    .any(|p| u16::from(p.0[2]) > u16::from(p.0[0]) + 40)
            );
            let numbering = xml(&bytes, "word/numbering.xml")?;
            for tag in ["abstractNum", "num"] {
                let attr = if tag == "num" {
                    "numId"
                } else {
                    "abstractNumId"
                };
                let ids: Vec<_> = numbering
                    .children
                    .iter()
                    .filter(|n| n.name == tag)
                    .filter_map(|n| n.attr(attr))
                    .collect();
                let unique: std::collections::BTreeSet<_> = ids.iter().collect();
                assert_eq!(ids.len(), unique.len());
            }
            let list: Vec<_> = body
                .children
                .iter()
                .filter(|n| n.path(&["pPr", "numPr"]).is_some())
                .collect();
            assert_eq!(list.len(), 4, "{body:?}");
            assert!(list.iter().any(|n| {
                n.path(&["pPr", "numPr", "ilvl"])
                    .and_then(|n| n.attr("val"))
                    == Some("1")
            }));
        }
        std::fs::create_dir(dir.0.join("render"))?;
        decode(
            &dir,
            "soffice",
            &[
                "-env:UserInstallation=file:///tmp/profile",
                "--headless",
                "--convert-to",
                "pdf",
                "--outdir",
                "/work/render",
                "/work/converted.docx",
            ],
        )?;
        let rendered = Document::load(dir.0.join("render/converted.pdf"))?;
        assert_eq!(
            rendered.get_pages().len(),
            original.get_pages().len(),
            "{name}"
        );
        decode(
            &dir,
            "pdftoppm",
            &[
                "-r",
                "60",
                "-singlefile",
                "-png",
                "/work/source.pdf",
                "/work/source",
            ],
        )?;
        decode(
            &dir,
            "pdftoppm",
            &[
                "-r",
                "60",
                "-singlefile",
                "-png",
                "/work/render/converted.pdf",
                "/work/converted",
            ],
        )?;
        decode(
            &dir,
            "magick",
            &[
                "/work/source.png",
                "/work/converted.png",
                "+append",
                "/work/compare.png",
            ],
        )?;
        if chart {
            let page = original.get_pages().into_values().next().ok_or("page")?;
            let height = pdfedit::page_bounds(&original, page)?.media[3];
            let top =
                ((height - positions(&original, "Revenue trend")?.1) * 60.0 / 72.0 + 8.0) as u32;
            let bottom =
                ((height - positions(&original, "Figure 1.")?.1) * 60.0 / 72.0 - 8.0) as u32;
            let ink = |image: image::DynamicImage| -> Result<[i32; 4]> {
                let rgb = image.to_rgb8();
                let mut rect: Option<[i32; 4]> = None;
                for (x, y, p) in rgb.enumerate_pixels().filter(|(_, y, p)| {
                    *y > top && *y < bottom && p.0[0] < 80 && p.0[1] < 130 && p.0[2] > 120
                }) {
                    let _ = p;
                    let (x, y) = (x as i32, y as i32);
                    rect = Some(rect.map_or([x, y, x, y], |r| {
                        [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)]
                    }));
                }
                rect.ok_or_else(|| "chart missing from rendered PNG".into())
            };
            let a = ink(image::open(dir.0.join("source.png"))?)?;
            let b = ink(image::open(dir.0.join("converted.png"))?)?;
            for (a, b) in a.into_iter().zip(b) {
                assert!(
                    (a - b).abs() <= 2,
                    "chart moved or changed width: {a} vs {b}"
                );
            }
        }
        if let Some(out) = std::env::var_os("DOCS_TEST_ARTIFACTS") {
            let out = std::path::PathBuf::from(out).join(name);
            std::fs::create_dir_all(&out)?;
            for file in [
                "source.pdf",
                "converted.docx",
                "source.png",
                "converted.png",
                "compare.png",
            ] {
                std::fs::copy(dir.0.join(file), out.join(file))?;
            }
            std::fs::copy(
                dir.0.join("render/converted.pdf"),
                out.join("converted.pdf"),
            )?;
        }
        let mut measurements = Vec::new();
        for key in keys {
            let a = positions(&original, key)?;
            let b = positions(&rendered, key)?;
            assert!(
                (a.0 - b.0).abs() <= 3.0 && (a.1 - b.1).abs() <= 3.0,
                "{name}: {key}: {a:?} vs {b:?}"
            );
            measurements.push(serde_json::json!({"text":key,"source_pt":[a.0,a.1],"writer_pt":[b.0,b.1],"tolerance_pt":3.0}));
        }
        if let Some(out) = std::env::var_os("DOCS_TEST_ARTIFACTS") {
            std::fs::write(
                std::path::PathBuf::from(out)
                    .join(name)
                    .join("positions.json"),
                serde_json::to_vec_pretty(&measurements)?,
            )?;
        }
    }
    Ok(())
}

fn bare_pdf(contents: &[&str]) -> Document {
    use lopdf::{Object, Stream, dictionary};
    let mut pdf = Document::with_version("1.5");
    let pages = pdf.new_object_id();
    let font = pdf.add_object(dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica", "Encoding"=>"WinAnsiEncoding"});
    let mut ids = Vec::new();
    for content in contents {
        let stream = pdf.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
        ids.push(pdf.add_object(dictionary! {"Type"=>"Page", "Parent"=>pages, "MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()], "Contents"=>stream,
            "Resources"=>dictionary! {"Font"=>dictionary! {"F1"=>font}}}));
    }
    pdf.objects.insert(pages, Object::Dictionary(dictionary! {"Type"=>"Pages", "Kids"=>ids.into_iter().map(Object::Reference).collect::<Vec<_>>(), "Count"=>contents.len() as i64}));
    let root = pdf.add_object(dictionary! {"Type"=>"Catalog", "Pages"=>pages});
    pdf.trailer.set("Root", root);
    pdf
}

#[test]
fn r12_unruled_numeric_table_ends_when_alignment_ends() -> Result {
    let pdf = bare_pdf(&[
        "BT /F1 12 Tf 1 0 0 1 50 700 Tm (Region) Tj 1 0 0 1 250 700 Tm (Value) Tj 1 0 0 1 50 680 Tm (North) Tj 1 0 0 1 250 680 Tm (42) Tj 1 0 0 1 50 660 Tm (South) Tj 1 0 0 1 250 660 Tm (84) Tj 1 0 0 1 50 640 Tm (West) Tj 1 0 0 1 250 640 Tm (90) Tj 1 0 0 1 50 610 Tm (After table) Tj ET",
    ]);
    let built = pdfdocx::build(&pdf, &pdf.get_pages().into_values().collect::<Vec<_>>())?;
    let root = xml(&built.data, "word/document.xml")?;
    let table = root.path(&["body", "tbl"]).ok_or("unruled table")?;
    assert_eq!(table.children.iter().filter(|n| n.name == "tr").count(), 4);
    assert!(!text(table).contains("After table"));
    assert!(text(root.path(&["body", "p"]).ok_or("following paragraph")?).contains("After table"));
    assert!(
        built
            .warnings
            .iter()
            .any(|w| w.code == MessageCode::PdfDocxTableInferred
                && w.params.get("page") == Some(&serde_json::json!(1)))
    );
    assert_eq!(
        table
            .path(&["tblPr", "tblBorders", "top"])
            .and_then(|n| n.attr("val")),
        Some("nil")
    );
    Ok(())
}

#[test]
fn r12_labelled_vector_is_rasterized_without_duplicate_text() -> Result {
    let pdf = bare_pdf(&[
        "q .16 .36 .57 rg 50 400 200 100 re f Q BT /F1 12 Tf 1 0 0 1 60 450 Tm (Label inside bar) Tj 1 0 0 1 50 350 Tm (Caption outside chart) Tj ET",
    ]);
    let pages: Vec<_> = pdf.get_pages().into_values().collect();
    let built = pdfdocx::build(&pdf, &pages)?;
    let content =
        String::from_utf8(docxxml::entry(&built.data, "word/document.xml")?.ok_or("document")?)?;
    assert!(!content.contains("Label inside bar"));
    assert!(content.contains("Caption outside chart"));
    assert_eq!(content.matches("<w:drawing>").count(), 1);
    assert!(content.contains("cx=\"2540000\""));
    assert!(
        built
            .warnings
            .iter()
            .any(|w| w.code == MessageCode::PdfDocxVectorText
                && w.params.get("page") == Some(&serde_json::json!(1)))
    );
    Ok(())
}

#[test]
fn r12_raster_limit_fails_before_write_and_keeps_previous_output() -> Result {
    let dir = Scratch::new()?;
    let mut pdf = bare_pdf(&["100 100 50 50 re f BT /F1 12 Tf 1 0 0 1 50 700 Tm (Title) Tj ET"]);
    let page = pdf.get_pages().into_values().next().ok_or("page")?;
    pdf.get_dictionary_mut(page)?.set(
        "MediaBox",
        vec![
            0.into(),
            0.into(),
            1_000_000_000.into(),
            1_000_000_000.into(),
        ],
    );
    pdf.save(dir.0.join("source.pdf"))?;
    std::fs::write(dir.0.join("out.docx"), b"previous output")?;
    let error = run(
        &serde_json::from_value(
            serde_json::json!({"op":"convert", "input":"source.pdf", "output":"out.docx"}),
        )?,
        &Options::new(dir.0.clone()),
    )
    .err()
    .ok_or("oversized raster accepted")?;
    assert_eq!(error.code(), MessageCode::PdfDocxRasterLimit);
    assert_eq!(error.params().get("page"), Some(&serde_json::json!(1)));
    assert_eq!(std::fs::read(dir.0.join("out.docx"))?, b"previous output");
    Ok(())
}

#[test]
fn r12_link_style_and_page_specific_unsupported_annotations() -> Result {
    use lopdf::{Object, dictionary};
    let mut pdf = bare_pdf(&[
        "BT /F1 12 Tf 1 0 0 1 50 700 Tm (Black link) Tj ET",
        "BT /F1 12 Tf 1 0 0 1 50 700 Tm (Second page) Tj ET",
    ]);
    let pages: Vec<_> = pdf.get_pages().into_values().collect();
    let link = pdf.add_object(dictionary! {"Type"=>"Annot", "Subtype"=>"Link", "Rect"=>vec![50.into(),697.into(),110.into(),712.into()], "A"=>dictionary! {"S"=>"URI", "URI"=>Object::string_literal("https://example.org/black")}});
    pdf.get_dictionary_mut(pages[0])?
        .set("Annots", vec![link.into()]);
    let unsupported = pdf.add_object(dictionary! {"Type"=>"Annot", "Subtype"=>"Text", "Rect"=>vec![50.into(),697.into(),100.into(),712.into()]});
    pdf.get_dictionary_mut(pages[1])?
        .set("Annots", vec![unsupported.into()]);
    let built = pdfdocx::build(&pdf, &pages)?;
    let root = xml(&built.data, "word/document.xml")?;
    let run = root
        .path(&["body", "p", "hyperlink", "r"])
        .ok_or("black link")?;
    assert_eq!(
        run.path(&["rPr", "color"]).and_then(|n| n.attr("val")),
        Some("000000")
    );
    assert!(run.path(&["rPr", "u"]).is_none());
    let warning = built
        .warnings
        .iter()
        .find(|w| w.code == MessageCode::PdfDocxAnnotationUnsupported)
        .ok_or("annotation warning")?;
    assert_eq!(warning.params.get("page"), Some(&serde_json::json!(2)));
    assert!(
        built
            .warnings
            .iter()
            .any(|w| w.code == MessageCode::PdfDocxFontSubstituted)
    );
    Ok(())
}

#[test]
fn r12_rgb_gray_cmyk_and_saved_colors_remain_run_properties() -> Result {
    let pdf = bare_pdf(&[
        "BT /F1 12 Tf 1 0 0 1 50 700 Tm 1 0 0 rg (Red) Tj q 0 .5 0 rg (Green) Tj Q (Red again) Tj .4 g (Gray) Tj 0 1 1 0 k (CMYK red) Tj ET",
    ]);
    let pages: Vec<_> = pdf.get_pages().into_values().collect();
    let lines = pdfedit::positioned_lines(&pdf, pages[0])?;
    assert_eq!(
        lines.iter().map(|l| l.color.as_str()).collect::<Vec<_>>(),
        ["FF0000", "008000", "FF0000", "666666", "FF0000"]
    );
    let built = pdfdocx::build(&pdf, &pages)?;
    let content =
        String::from_utf8(docxxml::entry(&built.data, "word/document.xml")?.ok_or("document")?)?;
    for color in ["FF0000", "008000", "666666"] {
        assert!(content.contains(&format!("w:color w:val=\"{color}\"")));
    }
    Ok(())
}
