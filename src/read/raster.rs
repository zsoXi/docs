//! Native OCR and isolated local decoding for legacy formats and PDF rasters.

use super::{Document, invalid, selection, word};
use crate::messages::{Msg, Params, params};
use crate::{Error, MessageCode, Options};
use crate::ocr::{Ocr, Page, page_result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Scratch(pub PathBuf);
impl Scratch {
    pub(crate) fn new() -> Result<Self, Error> {
        for _ in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "docs-read-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(Error::reader(MessageCode::ReadDecoder, e)),
            }
        }
        Err(Error::invalid(MessageCode::ReadDecoder, Params::new()))
    }
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        std::fs::write(self.0.join(name), bytes)
            .map_err(|e| Error::reader(MessageCode::ReadDecoder, e))
    }
    pub(crate) fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>, Error> {
        crate::path::read_limited(&self.0.join(name), name, limit)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Only system libraries/fonts and this request's scratch directory are visible.
/// Network, process, IPC and user namespaces are private; no caller paths reach a shell.
pub(crate) fn decode(scratch: &Scratch, program: &str, args: &[&str]) -> Result<(), Error> {
    let mut command = Command::new("bwrap");
    command.args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/usr",
        "/usr",
    ]);
    for system in ["/bin", "/lib", "/lib64", "/etc/fonts", "/etc/ld.so.cache"] {
        if Path::new(system).exists() {
            command.args(["--ro-bind", system, system]);
        }
    }
    command
        .args([
            "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--bind",
        ])
        .arg(&scratch.0)
        .args([
            "/work",
            "--chdir",
            "/work",
            "--clearenv",
            "--setenv",
            "HOME",
            "/tmp",
            "--setenv",
            "PATH",
            "/usr/bin",
            "--setenv",
            "LANG",
            "C.UTF-8",
            "--setenv",
            "SAL_USE_VCLPLUGIN",
            "svp",
            "--",
        ])
        .arg(format!("/usr/bin/{program}"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| Error::reader(MessageCode::ReadDecoder, e))?;
    let start = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| Error::reader(MessageCode::ReadDecoder, e))?
        {
            if status.success() {
                return Ok(());
            }
            return Err(Error::invalid(MessageCode::ReadDecoder, Params::new()));
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::invalid(MessageCode::ReadDecoder, Params::new()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn legacy(bytes: &[u8], ext: &str, opts: &Options) -> Result<Vec<u8>, Error> {
    let scratch = Scratch::new()?;
    let name = format!("in.{ext}");
    scratch.write(&name, bytes)?;
    std::fs::create_dir(scratch.0.join("out"))
        .map_err(|e| Error::reader(MessageCode::ReadDecoder, e))?;
    decode(
        &scratch,
        "soffice",
        &[
            "-env:UserInstallation=file:///tmp/profile",
            "--headless",
            "--nologo",
            "--nodefault",
            "--nofirststartwizard",
            "--convert-to",
            "docx:MS Word 2007 XML",
            "--outdir",
            "/work/out",
            &format!("/work/{name}"),
        ],
    )?;
    scratch.read("out/in.docx", opts.max_input_bytes)
}

pub(super) fn image_text(
    bytes: &[u8],
    ext: &str,
    opts: &Options,
    warnings: &mut Vec<Msg>,
) -> Result<Page, Error> {
    let Some(ocr) = Ocr::new(opts, warnings)? else {
        return Ok(Page::default());
    };
    if ext == "webp" {
        if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
            return Err(invalid());
        }
        Ok(page_result(ocr.recognize(bytes, opts), 1, opts))
    } else {
        if matches!(ext, "tif" | "tiff") {
            warnings.push(Msg::with(
                MessageCode::ReadFlattened,
                params(&[("feature", "tiff_frames".into())]),
            ));
        }
        Ok(page_result(ocr.recognize(bytes, opts), 1, opts))
    }
}

fn list_line<'a>(
    text: &str,
    lines: &'a [crate::pdfedit::PositionedLine],
    below: Option<f64>,
) -> Option<&'a crate::pdfedit::PositionedLine> {
    let text = text.trim();
    let item = text.strip_prefix("- ").or_else(|| {
        let (number, rest) = text.split_once(". ")?;
        (!number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit())).then_some(rest)
    })?;
    lines.iter().find(|line| {
        super::escape(line.text.trim()) == item && below.is_none_or(|y| line.y < y - 0.01)
    })
}

pub(super) fn pdf(bytes: &[u8], spec: Option<&str>, opts: &Options) -> Result<Document, Error> {
    let pdf = crate::pdfbudget::load(bytes)?;
    if pdf.is_encrypted() {
        return Err(invalid());
    }
    let pages: Vec<_> = pdf.get_pages().into_values().collect();
    let mut doc = Document {
        pages: Some(pages.len()),
        ..Document::default()
    };
    let mut pending = Vec::new();
    for index in selection(spec, pages.len())? {
        let page = pages[index - 1];
        let skipped = crate::pdfedit::form_warnings(&pdf, page)?;
        let partial = !skipped.is_empty();
        doc.warnings.extend(skipped);
        let lines = crate::pdfedit::positioned_lines(&pdf, page)?;
        let text = if partial && lines.iter().all(|l| l.text.trim().is_empty()) {
            // Do not reintroduce omitted forms through a raster/OCR fallback.
            String::new()
        } else if lines.iter().all(|l| l.text.trim().is_empty()) {
            pending.push(index);
            String::new()
        } else {
            // Reuse the existing page-layout reconstruction for structural inference.
            let built = crate::pdfdocx::build(&pdf, &[page])?;
            doc.warnings.extend(built.warnings.into_iter().filter(|w| w.code != MessageCode::PdfFormSkipped));
            let mut parsed = word::read(&built.data, None)?;
            let mut sizes: Vec<_> = lines.iter().map(|l| l.size).collect();
            sizes.sort_by(f64::total_cmp);
            let normal = sizes.get(sizes.len() / 2).copied().unwrap_or(11.0);
            for chunk in &mut parsed.chunks {
                // The layout backend pads face changes with NBSP; do not expose
                // that padding as a second Markdown word separator.
                chunk.text = chunk
                    .text
                    .replace(" \u{a0}", " ")
                    .replace("\u{a0} ", " ")
                    .replace('\u{a0}', " ");
                if let Some(line) = lines
                    .iter()
                    .find(|l| super::escape(l.text.trim()) == chunk.text.trim_matches('*').trim())
                    && line.size > normal * 1.35
                {
                    chunk.text.insert_str(0, "# ");
                }
                if let Some(rest) = chunk.text.strip_prefix("• ") {
                    chunk.text = format!("- {rest}");
                }
                chunk.text = chunk.text.replace("\n• ", "\n- ");
            }
            doc.warnings.extend(parsed.warnings);
            doc.warnings.push(Msg::with(
                MessageCode::ReadPdfStructure,
                params(&[("page", index.into())]),
            ));
            let mut text = String::new();
            let mut previous_list: Option<&crate::pdfedit::PositionedLine> = None;
            for chunk in parsed.chunks {
                let list = list_line(&chunk.text, &lines, previous_list.map(|l| l.y));
                let tight = previous_list.zip(list).is_some_and(
                    |(previous, current): (
                        &crate::pdfedit::PositionedLine,
                        &crate::pdfedit::PositionedLine,
                    )| {
                        let gap = previous.y - current.y;
                        gap > 0.0 && gap <= previous.size.max(current.size) * 2.05
                    },
                );
                if !text.is_empty() {
                    text.push_str(if tight { "\n" } else { "\n\n" });
                }
                text.push_str(&chunk.text);
                previous_list = list;
            }
            // The structural DOCX pass already carries URI links. Only add a
            // fallback for destinations absent from that result, never wrap an
            // existing Markdown link again (including wrapped/styled regions).
            let mut represented: std::collections::BTreeSet<String> =
                pulldown_cmark::Parser::new_ext(&text, pulldown_cmark::Options::all())
                    .filter_map(|event| match event {
                        pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) => Some(dest_url.to_string()),
                        _ => None,
                    }).collect();
            for annotation in pdf.get_page_annotations(page)? {
                let Ok(action) = annotation
                    .get(b"A")
                    .and_then(|v| pdf.dereference(v))
                    .and_then(|(_, v)| v.as_dict())
                else {
                    continue;
                };
                let Ok(uri) = action.get(b"URI").and_then(lopdf::decode_text_string) else {
                    continue;
                };
                if !represented.insert(uri.clone()) { continue; }
                let rect = annotation
                    .get(b"Rect")
                    .and_then(|v| v.as_array())
                    .ok()
                    .and_then(|a| {
                        let coords: Option<Vec<f32>> =
                            a.iter().map(|v| v.as_float().ok()).collect();
                        coords.filter(|v| v.len() == 4)
                    });
                let line = rect.as_ref().and_then(|r| {
                    lines.iter().find(|l| {
                        l.x >= f64::from(r[0]) - 2.0
                            && l.x + l.width <= f64::from(r[2]) + 2.0
                            && l.y >= f64::from(r[1]) - 2.0
                            && l.y <= f64::from(r[3]) + 2.0
                    })
                });
                let label = line
                    .map(|l| super::escape(l.text.trim()))
                    .unwrap_or_else(|| super::escape(&uri));
                let link = format!("[{label}]({})", word::destination(&uri));
                if line.is_some() && text.contains(&label) {
                    text = text.replacen(&label, &link, 1);
                } else {
                    text.push_str(&format!("\n\n{link}"));
                }
            }
            text
        };
        // Structural inference can omit numeric cells in unsupported layouts.
        // Audit exact lexemes against the native text layer, at source locations.
        let mut remaining = crate::numbers::tokens(&text);
        for (line_index, line) in lines.iter().enumerate() {
            for (token, count) in crate::numbers::tokens(&line.text) {
                let available = remaining.entry(token.clone()).or_default();
                let missing = count.saturating_sub(*available);
                *available = available.saturating_sub(count);
                if missing > 0 {
                    doc.warnings.push(Msg::with(MessageCode::ReadNumbersUnverified, params(&[
                        ("page",index.into()), ("line",(line_index+1).into()),
                        ("token",token.into()), ("missing_tokens",missing.into()),
                    ])));
                }
            }
        }
        doc.push(text, json!({"kind":"page","page":index}));
    }
    if !pending.is_empty() {
        let results = crate::ocr::pdf_pages(bytes, &pending, opts, &mut doc.warnings)?;
        for (index, page) in results {
            doc.warnings.extend(page.warnings);
            doc.ocr.push(crate::ocr::metadata(index, &page.lines));
            doc.push(page.text, json!({"kind":"page","page":index}));
        }
        doc.chunks.sort_by_key(|c| c.locator["page"].as_u64().unwrap_or(0));
    }
    Ok(doc)
}
