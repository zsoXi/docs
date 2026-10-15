//! Producer-to-consumer coverage for the complete link/image conversion matrix.
use super::*;
use std::error::Error as StdError;
use std::sync::atomic::{AtomicU64, Ordering};
type Result<T = ()> = std::result::Result<T, Box<dyn StdError>>;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "docs-r17-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn request(&self, value: serde_json::Value) -> Result<Output> {
        Ok(run(
            &serde_json::from_value(value)?,
            &Options::new(self.0.clone()),
        )?)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn descendants<'a>(node: &'a docxxml::Element, name: &str, out: &mut Vec<&'a docxxml::Element>) {
    if node.name == name {
        out.push(node);
    }
    for child in &node.children {
        descendants(child, name, out);
    }
}

pub(super) fn docx_image_size(bytes: &[u8]) -> Result<(f64, f64)> {
    let root = docxxml::parse(&docxxml::entry(bytes, "word/document.xml")?.ok_or("document")?)?;
    let mut extents = Vec::new();
    descendants(&root, "extent", &mut extents);
    assert_eq!(extents.len(), 1, "one embedded drawing required");
    let size = (
        extents[0].attr("cx").ok_or("cx")?.parse::<f64>()? / 9525.0,
        extents[0].attr("cy").ok_or("cy")?.parse::<f64>()? / 9525.0,
    );
    let parsed = docx::parse(bytes)?;
    assert_eq!(parsed.images.len(), 1, "one embedded raster required");
    assert!(!parsed.images[0].2.0.is_empty(), "empty image part");
    image::load_from_memory(&parsed.images[0].2.0)?;
    Ok(size)
}

fn pdf_image_size(dir: &Dir, output: &str) -> Result<(f64, f64)> {
    let pdf = pdfbudget::load(&std::fs::read(dir.0.join(output))?)?;
    assert_eq!(pdf.get_pages().len(), 1);
    let page = pdf.get_pages().into_values().next().ok_or("page")?;
    assert_eq!(
        pdfedit::page_images(&pdf, page)?.len(),
        1,
        "embedded raster required"
    );
    let rendered = std::process::Command::new("pdftoppm")
        .args(["-r", "96", "-singlefile", "-png", output, "raster"])
        .current_dir(&dir.0)
        .output()?;
    assert!(rendered.status.success(), "{rendered:?}");
    let raster = image::open(dir.0.join("raster.png"))?.to_rgb8();
    let mut bounds = (u32::MAX, u32::MAX, 0, 0);
    let mut count = 0;
    for (x, y, p) in raster.enumerate_pixels() {
        if p[0] > 180 && p[1] < 60 && p[2] < 60 {
            bounds = (
                bounds.0.min(x),
                bounds.1.min(y),
                bounds.2.max(x),
                bounds.3.max(y),
            );
            count += 1;
        }
    }
    assert!(count > 0, "raster image not visible");
    // Measure the display transform, not antialiased edge coverage: Poppler
    // can paint an extra edge pixel on both sides of a fractional-size image.
    let mut matrix = [1.0f64, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut stack = Vec::new();
    let mut sizes = Vec::new();
    for op in crate::content::parse(&pdf.get_page_content(page))? {
        if op.is("q") {
            stack.push(matrix);
        }
        if op.is("Q") {
            matrix = stack.pop().ok_or("graphics stack")?;
        }
        if op.is("cm") {
            let b = op.tail_numbers::<6>().ok_or("matrix")?;
            let a = matrix;
            matrix = [
                a[0] * b[0] + a[2] * b[1],
                a[1] * b[0] + a[3] * b[1],
                a[0] * b[2] + a[2] * b[3],
                a[1] * b[2] + a[3] * b[3],
                a[0] * b[4] + a[2] * b[5] + a[4],
                a[1] * b[4] + a[3] * b[5] + a[5],
            ];
        }
        if op.is("Do") {
            sizes.push((
                matrix[0].hypot(matrix[1]) / 0.75,
                matrix[2].hypot(matrix[3]) / 0.75,
            ));
        }
    }
    assert_eq!(sizes.len(), 1);
    Ok(sizes[0])
}
fn near(actual: (f64, f64), expected: (f64, f64), context: &str) {
    assert!(
        (actual.0 - expected.0).abs() <= 1.0 && (actual.1 - expected.1).abs() <= 1.0,
        "{context}: actual {actual:?}, expected {expected:?}"
    );
}

fn check_pdf_links(dir: &Dir, output: &str) -> Result {
    let bytes = std::fs::read(dir.0.join(output))?;
    let pdf = pdfbudget::load(&bytes)?;
    let page = pdf.get_pages().into_values().next().ok_or("page")?;
    let annots = pdf.get_page_annotations(page)?;
    let mut urls = Vec::new();
    let mut positions = Vec::new();
    for annotation in annots {
        let action = annotation.get(b"A")?.as_dict()?;
        let url = lopdf::decode_text_string(action.get(b"URI")?)?;
        let rect = annotation.get(b"Rect")?.as_array()?;
        let rect: Vec<f32> = rect
            .iter()
            .map(|v| v.as_float())
            .collect::<std::result::Result<_, _>>()?;
        assert!(rect[2] > rect[0] && rect[3] > rect[1], "{rect:?}");
        assert!(
            rect[0] >= 0.0 && rect[1] >= 0.0 && rect[2] < 600.0 && rect[3] < 850.0,
            "{rect:?}"
        );
        positions.push((url.clone(), rect));
        urls.push(url);
    }
    for url in ["https://example.org/a?q=1&b=2", "https://example.org/b"] {
        assert!(urls.iter().any(|u| u == url), "{output}: {urls:?}");
    }
    // Repeated labels must keep distinct, correctly ordered positions.
    let first = positions
        .iter()
        .find(|(u, _)| u.contains("a?q="))
        .ok_or("first URI")?;
    let second = positions
        .iter()
        .find(|(u, _)| u.ends_with("/b"))
        .ok_or("second URI")?;
    assert!(first.1[0] < second.1[0], "{positions:?}");
    let read = dir.request(serde_json::json!({"op":"read", "input":output}))?;
    let text = read.text.ok_or("reader text")?;
    assert!(text.contains("https://example.org/a?q=1&b=2"), "{text}");
    assert!(text.contains("https://example.org/b"), "{text}");
    assert!(
        !text.contains('\u{200b}')
            && !text.contains('\u{2060}')
            && !text.contains('\u{200c}')
            && !text.contains('\u{200d}'),
        "layout anchor leaked: {text:?}"
    );
    match std::process::Command::new("pdfinfo")
        .args(["-url", output])
        .current_dir(&dir.0)
        .output()
    {
        Ok(consumer) => {
            assert!(consumer.status.success(), "{consumer:?}");
            let text = String::from_utf8(consumer.stdout)?;
            assert!(
                text.contains("https://example.org/a?q=1&b=2")
                    && text.contains("https://example.org/b"),
                "{text}"
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("pdfinfo unavailable; native reader verified URLs")
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[test]
fn r17_link_image_matrix_preserves_urls_and_author_dimensions() -> Result {
    let dir = Dir::new()?;
    image::RgbImage::from_pixel(960, 334, image::Rgb([220, 20, 30]))
        .save(dir.0.join("chart.png"))?;
    for (case, author, expected) in [
        ("authored", "width=\"240\" height=\"83.5\"", (240.0, 83.5)),
        (
            "css",
            "width=\"999\" style=\"width:180pt;height:62.625pt\"",
            (240.0, 83.5),
        ),
        (
            "bounded",
            "width=\"1600\" height=\"800\"",
            (170.0 * 96.0 / 25.4, 85.0 * 96.0 / 25.4),
        ),
    ] {
        let links =
            "[Reference](https://example.org/a?q=1&b=2) and [Reference](https://example.org/b).";
        let markdown =
            format!("# Quarterly report\n\n{links}\n\n<img src=\"chart.png\" {author}>\n");
        std::fs::write(dir.0.join("in.md"), &markdown)?;
        std::fs::write(
            dir.0.join("in.html"),
            format!(
                "<style>@page {{size:A4;margin:20mm}}</style><h1>Quarterly report</h1><p><a href=\"https://example.org/a?q=1&amp;b=2\">Reference</a> and <a href=\"https://example.org/b\">Reference</a>.</p><img src=\"chart.png\" {author}>"
            ),
        )?;
        // The DOCX input is produced independently of the Markdown/HTML writer.
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            960,
            334,
            image::Rgb([220, 20, 30]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)?;
        let w = if case == "bounded" { 1600.0 } else { 240.0 };
        let h = if case == "bounded" { 800.0 } else { 83.5 };
        let doc = docx_rs::Docx::new()
            .page_size(11906, 16838)
            .page_margin(
                docx_rs::PageMargin::new()
                    .top(1134)
                    .bottom(1134)
                    .left(1134)
                    .right(1134),
            )
            .add_paragraph(
                docx_rs::Paragraph::new()
                    .add_hyperlink(
                        docx_rs::Hyperlink::new(
                            "https://example.org/a?q=1&b=2",
                            docx_rs::HyperlinkType::External,
                        )
                        .add_run(docx_rs::Run::new().add_text("Reference")),
                    )
                    .add_run(docx_rs::Run::new().add_text(" and "))
                    .add_hyperlink(
                        docx_rs::Hyperlink::new(
                            "https://example.org/b",
                            docx_rs::HyperlinkType::External,
                        )
                        .add_run(docx_rs::Run::new().add_text("Reference")),
                    ),
            )
            .add_paragraph(
                docx_rs::Paragraph::new().add_run(
                    docx_rs::Run::new().add_image(
                        docx_rs::Pic::new_with_dimensions(png.into_inner(), 960, 334)
                            .size((w * 9525.0) as u32, (h * 9525.0) as u32),
                    ),
                ),
            );
        std::fs::write(dir.0.join("in.docx"), docx::pack(doc)?)?;
        for input in ["in.md", "in.html", "in.docx"] {
            for extension in ["pdf", "docx"] {
                let output = format!("{case}-{}.{extension}", input.replace('.', "-"));
                let result = dir
                    .request(serde_json::json!({"op":"convert", "input":input, "output":output}))?;
                assert!(
                    !result
                        .warnings
                        .iter()
                        .any(|w| w.code.contains("images_docx") || w.code == "pdf.links_omitted"),
                    "{:?}",
                    result.warnings
                );
                if extension == "pdf" {
                    check_pdf_links(&dir, &output)?;
                    near(
                        pdf_image_size(&dir, &output)?,
                        expected,
                        &format!("{case} {input}→PDF"),
                    );
                } else {
                    let bytes = std::fs::read(dir.0.join(&output))?;
                    let size = docx_image_size(&bytes)?;
                    near(size, expected, &format!("{case} {input}→DOCX"));
                    let native = dir
                        .request(serde_json::json!({"op":"read", "input":output}))?
                        .text
                        .ok_or("DOCX reader")?;
                    assert!(
                        native.contains("https://example.org/a?q=1&b=2")
                            && native.contains("https://example.org/b"),
                        "{native}"
                    );
                    let rels = String::from_utf8(
                        docxxml::entry(&bytes, "word/_rels/document.xml.rels")?
                            .ok_or("relationships")?,
                    )?;
                    assert!(
                        rels.contains("https://example.org/a?q=1&amp;b=2")
                            && rels.contains("https://example.org/b"),
                        "{rels}"
                    );
                }
                if case == "authored"
                    && let Some(path) = std::env::var_os("DOCS_TEST_ARTIFACTS")
                {
                    std::fs::create_dir_all(&path)?;
                    std::fs::copy(dir.0.join(&output), PathBuf::from(&path).join(&output))?;
                    if extension == "pdf" {
                        std::fs::copy(
                            dir.0.join("raster.png"),
                            PathBuf::from(&path).join(format!("{output}.png")),
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn r17_wrapped_styled_and_image_links_keep_their_regions() -> Result {
    let dir = Dir::new()?;
    image::RgbImage::from_pixel(100, 50, image::Rgb([220, 20, 30]))
        .save(dir.0.join("chart.png"))?;
    let markdown = format!(
        "# [Linked heading](https://example.org/heading)\n\n[{} **bold** end](https://example.org/wrapped)\n\n[![Chart](chart.png)](https://example.org/image)\n\n```html\n<img src=\"never.png\">\n```",
        "wrapping words ".repeat(40)
    );
    for extension in ["pdf", "docx"] {
        let output = format!("styled.{extension}");
        dir.request(serde_json::json!({"op":"create", "markdown":markdown, "output":output}))?;
        let read = dir
            .request(serde_json::json!({"op":"read", "input":output}))?
            .text
            .ok_or("text")?;
        let plain: String = pulldown_cmark::Parser::new(&read)
            .filter_map(|event| match event {
                pulldown_cmark::Event::Text(text) | pulldown_cmark::Event::Code(text) => {
                    Some(text.to_string())
                }
                _ => None,
            })
            .collect();
        assert!(plain.contains("<img src=\"never.png\">"), "{read}");
        assert!(!read.contains("[["), "links must not nest: {read}");
        for suffix in ["heading", "wrapped", "image"] {
            assert!(
                read.contains(&format!("https://example.org/{suffix}")),
                "{read}"
            );
        }
        if extension == "pdf" {
            let pdf = pdfbudget::load(&std::fs::read(dir.0.join(&output))?)?;
            let page = pdf.get_pages().into_values().next().ok_or("page")?;
            let mut wrapped = 0;
            let mut image_rect = None;
            for annotation in pdf.get_page_annotations(page)? {
                let uri = lopdf::decode_text_string(annotation.get(b"A")?.as_dict()?.get(b"URI")?)?;
                if uri.ends_with("/wrapped") {
                    wrapped += 1;
                }
                if uri.ends_with("/image") {
                    let r = annotation.get(b"Rect")?.as_array()?;
                    image_rect = Some((
                        f64::from(r[2].as_float()? - r[0].as_float()?),
                        f64::from(r[3].as_float()? - r[1].as_float()?),
                    ));
                }
            }
            assert!(wrapped > 1, "wrapped link needs multiple line rectangles");
            near(
                image_rect.ok_or("image link annotation")?,
                (75.0, 37.5),
                "image URI region (points)",
            );
        }
    }
    Ok(())
}

#[test]
fn r17_parent_path_guard_retains_its_code() -> Result {
    let dir = Dir::new()?;
    assert!(matches!(
        path::resolve(&dir.0, "../outside.png"),
        Err(Error::PathEscapes { .. })
    ));
    Ok(())
}

#[test]
fn r17_missing_and_unreadable_images_warn_for_create_and_convert() -> Result {
    let dir = Dir::new()?;
    std::fs::write(dir.0.join("invalid.png"), b"not an image")?;
    for source in ["missing.png", "invalid.png"] {
        let markdown = format!("# Report\n\n![Chart]({source})");
        std::fs::write(dir.0.join("in.md"), &markdown)?;
        std::fs::write(
            dir.0.join("in.html"),
            format!("<p>Report</p><img src=\"{source}\">"),
        )?;
        for extension in ["pdf", "docx"] {
            for op in [
                serde_json::json!({"op":"create", "markdown":markdown, "output":format!("create.{extension}")}),
                serde_json::json!({"op":"convert", "input":"in.md", "output":format!("convert-md.{extension}")}),
                serde_json::json!({"op":"convert", "input":"in.html", "output":format!("convert-html.{extension}")}),
            ] {
                let out = dir.request(op)?;
                assert!(
                    out.warnings.iter().any(|w| w.code == "image.unreadable"),
                    "{out:?}"
                );
            }
        }
    }
    Ok(())
}
