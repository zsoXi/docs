#![allow(dead_code)]
use docs::{Input, Options, Output, run};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
pub struct Dir(pub PathBuf);
impl Dir {
    pub fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "docs-accept-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
    pub fn request(&self, request: Value) -> Result<Output> {
        Ok(run(
            &serde_json::from_value::<Input>(request)?,
            &Options::new(&self.0),
        )?)
    }
    pub fn read(&self, name: &str) -> Result<Output> {
        self.request(json!({"op":"read","input":name,"max_chars":100000,"locators":true}))
    }
    pub fn repeat(&self, request: Value) -> Result<Output> {
        let first = self.request(request.clone())?;
        let bytes = first
            .output
            .as_ref()
            .map(|p| std::fs::read(self.0.join(p)))
            .transpose()?;
        let second = self.request(request)?;
        assert_eq!(
            serde_json::to_value(&first)?,
            serde_json::to_value(&second)?,
            "I2 JSON"
        );
        if let (Some(before), Some(path)) = (bytes, second.output.as_ref()) {
            let after = std::fs::read(self.0.join(path))?;
            if before != after && std::env::var_os("DOCS_TEST_KEEP").is_some() {
                std::fs::write(self.0.join(format!("before-{path}")), &before)?;
            }
            assert!(
                before == after,
                "I2 bytes: {path}, lengths {} vs {}",
                before.len(),
                after.len()
            );
        }
        Ok(first)
    }
    pub fn report(&self) -> Result {
        image::RgbImage::from_pixel(640, 240, image::Rgb([220, 20, 30]))
            .save(self.0.join("chart.png"))?;
        std::fs::write(self.0.join("report.md"), REPORT)?;
        Ok(())
    }
    pub fn native_docx(&self) -> Result<Output> {
        self.report()?;
        self.repeat(json!({"op":"convert","input":"report.md","output":"report.docx"}))
    }
    pub fn native_pdf(&self) -> Result<Output> {
        self.report()?;
        self.repeat(json!({"op":"convert","input":"report.md","output":"report.pdf"}))
    }
    pub fn lo(&self) -> Result {
        self.report()?;
        let mut html = String::new();
        pulldown_cmark::html::push_html(
            &mut html,
            pulldown_cmark::Parser::new_ext(REPORT, pulldown_cmark::Options::all()),
        );
        std::fs::write(
            self.0.join("independent.html"),
            format!("<html><head><meta charset=\"utf-8\"></head><body>{html}</body></html>"),
        )?;
        // LibreOffice otherwise leaves local HTML images as external file links.
        // Embed the same source bytes before importing the Markdown-derived HTML.
        let embedded = Command::new("python3").args(["-c", "import base64,pathlib;p=pathlib.Path('independent.html');p.write_text(p.read_text().replace('src=\"chart.png\"','src=\"data:image/png;base64,'+base64.b64encode(pathlib.Path('chart.png').read_bytes()).decode()+'\"'))"]).current_dir(&self.0).output()?;
        assert!(embedded.status.success(), "{embedded:?}");
        let profile = format!(
            "-env:UserInstallation=file://{}",
            self.0.join("lo-profile").display()
        );
        for (source, format) in [
            ("independent.html", "docx:Office Open XML Text"),
            ("independent.docx", "pdf:writer_pdf_Export"),
        ] {
            let out = Command::new("timeout")
                .args([
                    "60",
                    "soffice",
                    &profile,
                    "--headless",
                    "--convert-to",
                    format,
                    "--outdir",
                    ".",
                    source,
                ])
                .current_dir(&self.0)
                .output()?;
            assert!(out.status.success(), "LibreOffice: {out:?}");
        }
        assert!(self.0.join("independent.docx").is_file());
        assert!(self.0.join("independent.pdf").is_file());
        Ok(())
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        if std::env::var_os("DOCS_TEST_KEEP").is_some() {
            eprintln!("Test artifacts: {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
pub const REPORT: &str = "# Quarterly Business Report\n\nCedar Consulting presents **approved revenue** and *reviewed forecasts*. See [audit policy](https://example.org/audit).\n\n## Financial results\n\n### Revenue outlook\n\n- Retain enterprise accounts\n- Review exposure\n\n3. Approve budget\n4. Notify finance\n\n| Metric | Value |\n| --- | --- |\n| Revenue | $1,234.50 |\n| Change | -12.5% |\n| Provision | (3,400) |\n| Reporting date | 2026-03-31 |\n| Invoice ID | 1234567890123456 |\n\n<img src=\"chart.png\" width=\"240\" height=\"90\" alt=\"Revenue chart\">\n\n```text\nstatus = approved\n```\n";
pub const NUMBERS: &[&str] = &[
    "$1,234.50",
    "-12.5%",
    "(3,400)",
    "2026-03-31",
    "1234567890123456",
];
pub fn content(out: &Output) -> Result<&str> {
    let text = out.text.as_deref().ok_or("missing text")?;
    for value in [
        "Quarterly Business Report",
        "approved revenue",
        "reviewed forecasts",
        "Financial results",
        "Revenue outlook",
        "Retain enterprise accounts",
        "Approve budget",
        "status = approved",
    ]
    .into_iter()
    .chain(NUMBERS.iter().copied())
    {
        assert!(text.contains(value), "missing {value:?}: {text}");
    }
    Ok(text)
}
pub fn zip_xml(dir: &Dir, name: &str, entry: &str) -> Result<String> {
    let out = Command::new("python3").args(["-c", "import sys,zipfile;sys.stdout.buffer.write(zipfile.ZipFile(sys.argv[1]).read(sys.argv[2]))", name, entry]).current_dir(&dir.0).output()?;
    assert!(out.status.success(), "{out:?}");
    Ok(String::from_utf8(out.stdout)?)
}
pub fn pdf_checks(
    dir: &Dir,
    name: &str,
    expected_pages: Option<usize>,
    size: Option<(f32, f32)>,
) -> Result {
    let doc = lopdf::Document::load(dir.0.join(name))?;
    if let Some(n) = expected_pages {
        assert_eq!(doc.get_pages().len(), n, "{name}");
    }
    let mut urls = Vec::new();
    let mut image_sizes = Vec::new();
    for page in doc.get_pages().into_values() {
        for annot in doc.get_page_annotations(page)? {
            urls.push(lopdf::decode_text_string(
                annot.get(b"A")?.as_dict()?.get(b"URI")?,
            )?);
        }
        let ops = lopdf::content::Content::decode(&doc.get_page_content(page))?;
        let mut matrix = [1.0f32, 0., 0., 1., 0., 0.];
        let mut stack = Vec::new();
        for op in ops.operations {
            match op.operator.as_str() {
                "q" => stack.push(matrix),
                "Q" => matrix = stack.pop().ok_or("matrix stack")?,
                "cm" => {
                    let b: Vec<f32> = op
                        .operands
                        .iter()
                        .map(|x| x.as_float())
                        .collect::<std::result::Result<_, _>>()?;
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
                "Do" => image_sizes.push((
                    matrix[0].hypot(matrix[1]) / 0.75,
                    matrix[2].hypot(matrix[3]) / 0.75,
                )),
                _ => {}
            }
        }
    }
    assert!(
        urls.iter().any(|u| u == "https://example.org/audit"),
        "{name}: {urls:?}"
    );
    if let Some((w, h)) = size {
        assert!(
            image_sizes
                .iter()
                .any(|(x, y)| (x - w).abs() <= 1. && (y - h).abs() <= 1.),
            "{name}: {image_sizes:?}"
        );
    }
    for (tool, args) in [
        ("pdftotext", vec![name, "-"]),
        ("pdfinfo", vec!["-url", name]),
    ] {
        match Command::new(tool).args(args).current_dir(&dir.0).output() {
            Ok(out) => {
                assert!(out.status.success(), "{tool}: {out:?}");
                let text = String::from_utf8(out.stdout)?;
                if tool == "pdftotext" {
                    for n in NUMBERS {
                        assert!(text.contains(n), "{tool}: {n}: {text}");
                    }
                } else {
                    assert!(text.contains("https://example.org/audit"), "{text}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("SKIP: {tool} unavailable; native consumer checked")
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
