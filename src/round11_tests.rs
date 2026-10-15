//! End-to-end regressions for repeated edits and document conversion boundaries.

use super::*;
use std::collections::BTreeSet;
use std::error::Error as StdError;
use std::sync::atomic::{AtomicU64, Ordering};
type TestResult = Result<(), Box<dyn StdError>>;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "docs-r11-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn opts(&self) -> Options {
        Options::new(self.0.clone())
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn create(dir: &Dir, markdown: &str, output: &str) -> Result<Output, Error> {
    run(
        &Input::Create {
            markdown: markdown.into(),
            output: output.into(),
        },
        &dir.opts(),
    )
}
fn replace(
    dir: &Dir,
    input: &str,
    output: &str,
    find: &str,
    replacement: &str,
) -> Result<Output, Error> {
    run(
        &Input::Replace {
            input: input.into(),
            output: output.into(),
            find: find.into(),
            replace: replacement.into(),
            all: true,
        },
        &dir.opts(),
    )
}
fn convert(dir: &Dir, input: &str, output: &str) -> Result<Output, Error> {
    run(
        &Input::Convert {
            input: input.into(),
            output: output.into(),
        },
        &dir.opts(),
    )
}
fn word_reference(dir: &Dir, input: &str, folder: &str) -> Result<PathBuf, Box<dyn StdError>> {
    let reference = dir.0.join(folder);
    std::fs::create_dir(&reference)?;
    let profile = format!(
        "-env:UserInstallation=file://{}",
        dir.0.join(format!("profile-{folder}")).display()
    );
    let output = std::process::Command::new("timeout")
        .args([
            "45",
            "libreoffice",
            &profile,
            "--headless",
            "--convert-to",
            "pdf",
            "--outdir",
        ])
        .arg(&reference)
        .arg(dir.0.join(input))
        .output()?;
    assert!(output.status.success(), "{:?}", output.stderr);
    Ok(reference.join(Path::new(input).with_extension("pdf")))
}

fn poppler(path: &Path) -> Result<String, Box<dyn StdError>> {
    let out = std::process::Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .output()?;
    assert!(out.status.success(), "{:?}", out.stderr);
    Ok(String::from_utf8(out.stdout)?)
}

#[test]
fn r11_two_pdf_edits_preserve_both_subsets() -> TestResult {
    let dir = Dir::new()?;
    create(
        &dir,
        "Northstar Analytics Ltd\n\nFee: 1,500.00",
        "source.pdf",
    )?;
    for inplace in [false, true] {
        let first = if inplace { "same.pdf" } else { "first.pdf" };
        let second = if inplace { "same.pdf" } else { "second.pdf" };
        replace(
            &dir,
            "source.pdf",
            first,
            "Northstar Analytics Ltd",
            "Cedar Consulting Ltd",
        )?;
        assert!(poppler(&dir.0.join(first))?.contains("Cedar Consulting Ltd"));
        replace(&dir, first, second, "1,500.00", "1,750.00")?;
        let text = poppler(&dir.0.join(second))?;
        assert!(text.contains("Cedar Consulting Ltd"), "{text}");
        assert!(text.contains("1,750.00"), "{text}");
        assert!(!text.contains("1,500.00"));
    }
    Ok(())
}

pub(super) fn numbering(bytes: &[u8]) -> Result<(), Box<dyn StdError>> {
    let xml = docxxml::entry(bytes, "word/numbering.xml")?.ok_or("numbering")?;
    let root = docxxml::parse(&xml)?;
    let mut abstracts = BTreeSet::new();
    let mut nums = BTreeSet::new();
    for child in &root.children {
        if child.name == "abstractNum" {
            assert!(
                abstracts.insert(child.attr("abstractNumId").ok_or("abstract ID")?),
                "duplicate abstract ID"
            );
        }
        if child.name == "num" {
            assert!(
                nums.insert(child.attr("numId").ok_or("num ID")?),
                "duplicate num ID"
            );
        }
    }
    for child in root.children.iter().filter(|n| n.name == "num") {
        assert!(abstracts.contains(child.value("abstractNumId").ok_or("reference")?));
    }
    let document = docxxml::parse(&docxxml::entry(bytes, "word/document.xml")?.ok_or("document")?)?;
    fn references(node: &docxxml::Element, ids: &BTreeSet<&str>) {
        if node.name == "numId" {
            assert!(node.attr("val").is_some_and(|id| ids.contains(id)));
        }
        for child in &node.children {
            references(child, ids);
        }
    }
    references(&document, &nums);
    Ok(())
}
#[test]
fn r11_list_ids_start_and_repack_are_consistent() -> TestResult {
    let dir = Dir::new()?;
    create(
        &dir,
        "# CV\n\n- Alpha\n- Beta\n\n7. Seven\n8. Eight",
        "cv.docx",
    )?;
    for input in ["cv.docx", "edited.docx", "again.docx"] {
        if input == "edited.docx" {
            replace(&dir, "cv.docx", input, "Alpha", "Gamma")?;
        }
        if input == "again.docx" {
            replace(&dir, "edited.docx", input, "Gamma", "Delta")?;
        }
        let bytes = std::fs::read(dir.0.join(input))?;
        numbering(&bytes)?;
        let html = docxhtml::convert(&bytes, &docx::parse(&bytes)?)?.body;
        assert!(html.contains("• "), "{html}");
        assert!(html.contains("7. "), "{html}");
        assert!(html.contains("8. "), "{html}");
        let rendered = poppler(&word_reference(&dir, input, &format!("render-{input}"))?)?;
        assert!(rendered.contains('•'), "{rendered}");
        assert!(
            rendered.contains("7.") && rendered.contains("8."),
            "{rendered}"
        );
        assert!(!rendered.contains("1."), "{rendered}");
    }
    Ok(())
}
#[test]
fn r11_create_and_convert_report_embed_images() -> TestResult {
    let dir = Dir::new()?;
    let markdown = "# Quarterly report\n\n![Quarterly chart](chart.png)";
    let image = image::RgbImage::from_pixel(4, 4, image::Rgb([30, 80, 190]));
    image.save(dir.0.join("chart.png"))?;
    std::fs::write(dir.0.join("report.md"), markdown)?;
    for out in [
        create(&dir, markdown, "created.docx")?,
        convert(&dir, "report.md", "converted.docx")?,
    ] {
        assert!(!out.warnings.iter().any(|w| w.code == "convert.markdown_images_docx"));
        let bytes = std::fs::read(dir.0.join(out.output.ok_or("output")?))?;
        let (width, height) = crate::round17_tests::docx_image_size(&bytes)?;
        assert!((width - 4.0).abs() <= 1.0 && (height - 4.0).abs() <= 1.0);
    }
    Ok(())
}
#[cfg(unix)]
#[test]
fn r11_embedded_images_obey_path_type_and_size_rules() -> TestResult {
    let dir = Dir::new()?;
    std::os::unix::fs::symlink("missing.png", dir.0.join("image.png"))?;
    let error = create(&dir, "![Chart](image.png)", "out.pdf")
        .err()
        .ok_or("accepted image symlink")?;
    assert_eq!(error.code(), MessageCode::PathSymlink);
    std::fs::remove_file(dir.0.join("image.png"))?;
    std::fs::create_dir(dir.0.join("image.png"))?;
    let error = create(&dir, "![Chart](image.png)", "out.pdf")
        .err()
        .ok_or("accepted image directory")?;
    assert_eq!(error.code(), MessageCode::FileNotRegular);
    std::fs::remove_dir(dir.0.join("image.png"))?;
    std::fs::write(dir.0.join("image.png"), [0u8; 32])?;
    let mut opts = dir.opts();
    opts.max_input_bytes = 16;
    let error = run(
        &Input::Create {
            markdown: "![Chart](image.png)".into(),
            output: "out.pdf".into(),
        },
        &opts,
    )
    .err()
    .ok_or("accepted oversized image")?;
    assert_eq!(error.code(), MessageCode::FileTooLarge);
    assert!(!dir.0.join("out.pdf").exists());
    Ok(())
}

#[test]
fn r11_html_conversion_omits_non_content_elements() -> TestResult {
    let dir = Dir::new()?;
    std::fs::write(
        dir.0.join("in.html"),
        "<!doctype html><HTML><HEAD><title>Hidden title</title><STYLE>@page { margin:20mm }</STYLE><SCRIPT>secret_script()</SCRIPT></HEAD><body><h1>Visible heading</h1><p>Visible body</p><script>hidden_body_script()</script></body></HTML>",
    )?;
    for output in ["out.md", "out.docx"] {
        convert(&dir, "in.html", output)?;
        let bytes = std::fs::read(dir.0.join(output))?;
        let text = if output.ends_with("docx") {
            docx::read_text(&bytes)?
        } else {
            String::from_utf8(bytes)?
        };
        assert!(text.contains("Visible heading"));
        assert!(text.contains("Visible body"));
        for hidden in [
            "Hidden title",
            "@page",
            "secret_script",
            "hidden_body_script",
        ] {
            assert!(!text.contains(hidden), "{text}");
        }
    }
    Ok(())
}
#[test]
fn r11_invalid_utf8_preserves_inputs_and_outputs() -> TestResult {
    let dir = Dir::new()?;
    let bad = b"Client \xff; amount 100";
    for extension in ["txt", "md", "html"] {
        let name = format!("bad.{extension}");
        std::fs::write(dir.0.join(&name), bad)?;
        std::fs::write(dir.0.join("old.docx"), b"old output")?;
        let error = replace(&dir, &name, &name, "100", "200")
            .err()
            .ok_or("accepted invalid UTF-8")?;
        assert_eq!(error.code().as_str(), "input.utf8");
        assert!(StdError::source(&error).is_some());
        assert_eq!(std::fs::read(dir.0.join(&name))?, bad);
        let error = convert(&dir, &name, "old.docx")
            .err()
            .ok_or("converted invalid UTF-8")?;
        assert_eq!(error.code().as_str(), "input.utf8");
        assert_eq!(std::fs::read(dir.0.join("old.docx"))?, b"old output");
    }
    Ok(())
}
#[test]
fn r11_docx_error_retains_typed_cause() -> TestResult {
    let dir = Dir::new()?;
    std::fs::write(dir.0.join("bad.docx"), b"not a zip")?;
    let error = convert(&dir, "bad.docx", "out.md")
        .err()
        .ok_or("accepted bad DOCX")?;
    assert_eq!(error.code(), MessageCode::DocxRead);
    assert!(StdError::source(&error).is_some_and(|cause| cause.is::<docx_rs::ReaderError>()));
    assert!(!error.to_string().contains("not a zip"));
    let packing = Error::DocxBuild(Box::new(std::io::Error::other("internal packing detail")));
    assert_eq!(packing.code(), MessageCode::DocxBuild);
    assert!(StdError::source(&packing).is_some_and(|cause| cause.is::<std::io::Error>()));
    assert!(!packing.to_string().contains("internal packing detail"));
    Ok(())
}
#[test]
fn r11_parallel_updates_read_current_state() -> TestResult {
    let dir = Dir::new()?;
    for _ in 0..4 {
        std::fs::write(
            dir.0.join("state.txt"),
            format!("Client amount 100\n{}", "padding ".repeat(200_000)),
        )?;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [("Client", "Vendor"), ("100", "200")]
            .into_iter()
            .map(|(find, replacement)| {
                let options = dir.opts();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    run(
                        &Input::Replace {
                            input: "state.txt".into(),
                            output: "state.txt".into(),
                            find: find.into(),
                            replace: replacement.into(),
                            all: true,
                        },
                        &options,
                    )
                })
            })
            .collect();
        for thread in threads {
            thread.join().map_err(|_| "worker panicked")??;
        }
        assert!(std::fs::read_to_string(dir.0.join("state.txt"))?.starts_with("Vendor amount 200"));
    }
    Ok(())
}

#[test]
fn r11_parallel_pdf_edits_are_independent() -> TestResult {
    let threads: Vec<_> = (0..2)
        .map(|_| {
            std::thread::spawn(|| {
                r11_two_pdf_edits_preserve_both_subsets().map_err(|e| e.to_string())
            })
        })
        .collect();
    for thread in threads {
        thread
            .join()
            .map_err(|_| "worker panicked")?
            .map_err(std::io::Error::other)?;
    }
    Ok(())
}

#[test]
fn r11_split_merge_and_replace_detach_hardlink_outputs() -> TestResult {
    let dir = Dir::new()?;
    create(&dir, "# Original invoice\n\nAmount 100", "source.pdf")?;
    let original = std::fs::read(dir.0.join("source.pdf"))?;
    for op in ["split", "merge"] {
        std::fs::hard_link(dir.0.join("source.pdf"), dir.0.join("alias.pdf"))?;
        let request = if op == "split" {
            Input::Split {
                input: "source.pdf".into(),
                output: "alias.pdf".into(),
                pages: "1".into(),
            }
        } else {
            Input::Merge {
                inputs: vec!["source.pdf".into(), "source.pdf".into()],
                output: "alias.pdf".into(),
            }
        };
        run(&request, &dir.opts())?;
        assert_eq!(std::fs::read(dir.0.join("source.pdf"))?, original);
        let pdf = lopdf::Document::load(dir.0.join("alias.pdf"))?;
        assert_eq!(pdf.get_pages().len(), if op == "split" { 1 } else { 2 });
        std::fs::remove_file(dir.0.join("alias.pdf"))?;
    }
    std::fs::write(dir.0.join("source.txt"), "Client 100")?;
    std::fs::hard_link(dir.0.join("source.txt"), dir.0.join("alias.txt"))?;
    replace(&dir, "source.txt", "alias.txt", "100", "200")?;
    assert_eq!(std::fs::read(dir.0.join("source.txt"))?, b"Client 100");
    assert_eq!(std::fs::read(dir.0.join("alias.txt"))?, b"Client 200");
    Ok(())
}

#[test]
fn r11_diagnostic_values_are_bounded() -> TestResult {
    let dir = Dir::new()?;
    std::fs::write(dir.0.join("in.txt"), "Client 100")?;
    let output = replace(&dir, "in.txt", "out.txt", &"A".repeat(6000), "B")?;
    let json = serde_json::to_string(&output)?;
    assert!(json.len() < 1500, "{}", json.len());
    assert!(output.summary_params.contains_key("truncated_fields"));
    assert!(output.summary.len() < 500);
    std::fs::write(
        dir.0.join("locator.mhtml"),
        format!(
            "Content-Type: text/html\r\nContent-Location: {}\r\n\r\n<p>Visible body</p>",
            "x".repeat(6000)
        ),
    )?;
    let request = serde_json::from_value(
        serde_json::json!({"op":"read", "input":"locator.mhtml", "locators":true, "max_chars":100}),
    )?;
    let output = run(&request, &dir.opts())?;
    let data = output.data.ok_or("missing locators")?;
    assert_eq!(
        data["chunks"][0]["locator"]["truncated_fields"]["location"],
        6000
    );
    assert!(data.to_string().len() < 600);
    assert!(
        output
            .text
            .as_deref()
            .is_some_and(|text| text.contains("Visible body"))
    );
    Ok(())
}
#[cfg(unix)]
#[test]
fn r11_symlinks_including_dangling_are_rejected() -> TestResult {
    use std::os::unix::fs::symlink;
    let dir = Dir::new()?;
    std::fs::write(dir.0.join("real.txt"), "100")?;
    symlink("real.txt", dir.0.join("link.txt"))?;
    symlink("missing.txt", dir.0.join("dangling.txt"))?;
    symlink("missing-dir", dir.0.join("parent"))?;
    for path in [
        "link.txt",
        "dangling.txt",
        "parent/new.txt",
        "link.txt/../new.txt",
    ] {
        let error = replace(&dir, "real.txt", path, "100", "200")
            .err()
            .ok_or("accepted symlink")?;
        assert_eq!(error.code().as_str(), "path.symlink", "{path}: {error}");
    }
    assert_eq!(std::fs::read(dir.0.join("real.txt"))?, b"100");
    assert!(!dir.0.join("missing.txt").exists());
    Ok(())
}
#[test]
fn r11_word_spacing_and_cell_insets_match_independent_render() -> TestResult {
    let dir = Dir::new()?;
    create(
        &dir,
        "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.\n\n| Cell | Value |\n|---|---|\n| Data | 42 |\n| North | 12 |\n| South | 24 |\n| West | 36 |\n| Total | 114 |\n\nFinal paragraph.",
        "source.docx",
    )?;
    convert(&dir, "source.docx", "native.pdf")?;
    let reference = dir.0.join("reference");
    std::fs::create_dir(&reference)?;
    let profile = format!(
        "-env:UserInstallation=file://{}",
        dir.0.join("profile").display()
    );
    let status = std::process::Command::new("timeout")
        .args([
            "45",
            "libreoffice",
            &profile,
            "--headless",
            "--convert-to",
            "pdf",
            "--outdir",
        ])
        .arg(&reference)
        .arg(dir.0.join("source.docx"))
        .status()?;
    assert!(status.success());
    fn words(
        path: &Path,
    ) -> Result<std::collections::BTreeMap<String, (f64, f64)>, Box<dyn StdError>> {
        let out = std::process::Command::new("pdftotext")
            .arg("-bbox")
            .arg(path)
            .arg("-")
            .output()?;
        assert!(out.status.success());
        let root = docxxml::parse(&out.stdout)?;
        let mut map = std::collections::BTreeMap::new();
        fn collect(
            n: &docxxml::Element,
            map: &mut std::collections::BTreeMap<String, (f64, f64)>,
        ) -> TestResult {
            if n.name == "word" {
                map.insert(
                    n.text.split_whitespace().next().ok_or("empty word")?.into(),
                    (
                        n.attr("xMin").ok_or("x")?.parse()?,
                        n.attr("yMin").ok_or("y")?.parse()?,
                    ),
                );
            }
            for child in &n.children {
                collect(child, map)?;
            }
            Ok(())
        }
        collect(&root, &mut map)?;
        Ok(map)
    }
    let actual = words(&dir.0.join("native.pdf"))?;
    let expected = words(&reference.join("source.pdf"))?;
    for word in [
        "First", "Second", "Third", "Cell", "Data", "North", "South", "West", "Total", "Final",
    ] {
        let (x, y) = actual.get(word).ok_or("native word")?;
        let (rx, ry) = expected.get(word).ok_or("reference word")?;
        assert!(
            (x - rx).abs() <= 1.0,
            "{word}: native x {x}, reference {rx}"
        );
        assert!(
            (y - ry).abs() <= 1.0,
            "{word}: native y {y}, reference {ry}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn r11_hardlink_conversion_preserves_source() -> TestResult {
    let dir = Dir::new()?;
    create(&dir, "# Contract\n\nOriginal document", "source.pdf")?;
    let before = std::fs::read(dir.0.join("source.pdf"))?;
    std::fs::hard_link(dir.0.join("source.pdf"), dir.0.join("same-format.pdf"))?;
    let error = convert(&dir, "source.pdf", "same-format.pdf")
        .err()
        .ok_or("accepted same-format alias")?;
    assert_eq!(error.code(), MessageCode::ConvertOverwritesInput);
    std::fs::hard_link(dir.0.join("source.pdf"), dir.0.join("alias.docx"))?;
    convert(&dir, "source.pdf", "alias.docx")?;
    assert_eq!(std::fs::read(dir.0.join("source.pdf"))?, before);
    assert!(
        docx::read_text(&std::fs::read(dir.0.join("alias.docx"))?)?.contains("Original document")
    );
    Ok(())
}
