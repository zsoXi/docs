//! Read-only format adapters. No package resource is fetched or extracted to disk.

pub(crate) mod html;
pub(crate) mod ocr_layout;
mod office;
pub(crate) mod raster;
#[cfg(test)]
mod tests;
mod web;
mod word;
mod xml;

use crate::messages::{MessageCode, Msg, Params, params};
use crate::{Error, Options, Output, count_words, make_output, path};
use serde::Serialize;
use serde_json::{Value, json};

pub(crate) struct Request<'a> {
    pub input: &'a str,
    pub max_chars: usize,
    pub locators: bool,
    pub pages: Option<&'a str>,
    pub range: Option<&'a str>,
    pub max_rows: usize,
}

#[derive(Serialize)]
struct Chunk {
    text: String,
    locator: Value,
}

#[derive(Default)]
struct Document {
    chunks: Vec<Chunk>,
    warnings: Vec<Msg>,
    pages: Option<usize>,
    ocr: Vec<Value>,
}

impl Document {
    fn push(&mut self, text: impl Into<String>, locator: Value) {
        let text = text.into();
        if !text.trim().is_empty() {
            self.chunks.push(Chunk { text, locator });
        }
    }
}

pub(crate) fn invalid() -> Error {
    Error::invalid(MessageCode::ReadInvalid, Params::new())
}

fn selection(spec: Option<&str>, total: usize) -> Result<Vec<usize>, Error> {
    match spec {
        None => Ok((1..=total).collect()),
        Some(spec) => crate::pdf::parse_pages(spec, u32::try_from(total).map_err(|_| invalid())?)
            .map(|set| set.into_iter().map(|v| v as usize).collect())
            .map_err(|_| Error::invalid(MessageCode::ReadRange, params(&[("range", spec.into())]))),
    }
}

pub(crate) fn run(request: Request<'_>, opts: &Options) -> Result<Output, Error> {
    let path = path::resolve(&opts.base_dir, request.input)?;
    let bytes = path::read_limited(&path, request.input, opts.max_input_bytes)?;
    if bytes.is_empty() { return Err(Error::invalid(MessageCode::InputEmpty, params(&[("path",request.input.into())]))); }
    let ext = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "md" | "markdown") { crate::markdown::validate(crate::text_input(&bytes)?)?; }
    if request.pages.is_some() && request.range.is_some() {
        return Err(Error::invalid(
            MessageCode::ReadRange,
            params(&[("range", "pages + range".into())]),
        ));
    }
    if request.pages.is_some() && !matches!(ext.as_str(), "pdf" | "pptx" | "odp") {
        return Err(Error::invalid(
            MessageCode::ReadRange,
            params(&[("range", "pages".into())]),
        ));
    }
    let spec = request.pages.or(request.range);
    let mut doc = match ext.as_str() {
        "pdf" => raster::pdf(&bytes, spec, opts)?,
        "docx" => word::read(&bytes, spec)?,
        "doc" | "rtf" => {
            let converted = raster::legacy(&bytes, &ext, opts)?;
            let mut doc = word::read(&converted, spec)?;
            doc.warnings.push(Msg::with(
                MessageCode::ReadLegacy,
                params(&[("format", ext.clone().into())]),
            ));
            doc
        }
        "odt" | "odp" => office::odf(&bytes, &ext, spec)?,
        "pptx" => office::presentation(&bytes, spec)?,
        "xlsx" | "ods" | "csv" => {
            office::spreadsheet(&bytes, &ext, request.range, request.max_rows)?
        }
        "epub" => web::epub(&bytes, spec)?,
        "xmind" => web::xmind(&bytes, spec)?,
        "mhtml" | "mht" => web::mhtml(&bytes, spec)?,
        "html" | "htm" => {
            let text = html::markdown(
                std::str::from_utf8(&bytes)
                    .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?,
            )?;
            blocks(text, spec, "block")?
        }
        "md" | "markdown" | "txt" | "text" => blocks(
            String::from_utf8(bytes.clone())
                .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?,
            spec,
            "block",
        )?,
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" => {
            selection(spec, 1)?;
            let mut doc = Document::default();
            let page = raster::image_text(&bytes, &ext, opts, &mut doc.warnings)?;
            doc.warnings.extend(page.warnings);
            if !page.lines.is_empty() { doc.ocr.push(crate::ocr::metadata(1, &page.lines)); }
            doc.push(page.text, json!({"kind":"image", "image":1}));
            doc
        }
        _ => return Err(Error::UnsupportedFormat(ext)),
    };
    let mut full = String::new();
    for chunk in &mut doc.chunks {
        if !full.is_empty() {
            chunk.text.insert_str(0, "\n\n");
        }
        full.push_str(&chunk.text);
    }
    let words = count_words(&full);
    let total_chars = full.chars().count();
    if total_chars > request.max_chars {
        let mut remaining = request.max_chars;
        for chunk in &mut doc.chunks {
            let take = chunk.text.chars().count().min(remaining);
            chunk.text = chunk.text.chars().take(take).collect();
            remaining -= take;
        }
        doc.chunks.retain(|c| !c.text.is_empty());
        if let Some(last) = doc.chunks.last_mut() {
            last.text.push('…');
        }
        full = doc.chunks.iter().map(|c| c.text.as_str()).collect();
        doc.warnings.push(Msg::with(
            MessageCode::ReadTruncated,
            params(&[
                ("max_chars", request.max_chars.into()),
                ("total_chars", total_chars.into()),
                ("omitted_chars", (total_chars - request.max_chars).into()),
                ("returned_chars", full.chars().count().into()),
            ]),
        ));
    }
    let mut summary_params = params(&[("words", words.into()), ("path", request.input.into())]);
    let summary_code = if let Some(pages) = doc.pages {
        summary_params.insert("pages".into(), pages.into());
        MessageCode::ReadPages
    } else {
        MessageCode::ReadWords
    };
    let mut output = make_output(
        opts,
        Msg::with(summary_code, summary_params),
        Some(request.input.into()),
        Some(bytes.len() as u64),
        Some(full),
        doc.warnings,
    );
    if !doc.ocr.is_empty() {
        // Confidence and geometry are useful even when locators were not requested.
        output.data = Some(json!({"ocr":doc.ocr}));
    }
    if request.locators {
        for chunk in &mut doc.chunks {
            if let Some(fields) = chunk.locator.as_object_mut() {
                // Locator descriptions are metadata, not document text. Keep
                // their previews bounded independently of max_chars.
                *fields =
                    crate::messages::bounded_params(std::mem::take(fields).into_iter().collect())
                        .into_iter()
                        .collect();
            }
        }
        let data = output.data.get_or_insert_with(|| json!({}));
        data["chunks"] = json!(doc.chunks);
    }
    Ok(output)
}

fn blocks(text: String, spec: Option<&str>, kind: &str) -> Result<Document, Error> {
    let mut doc = Document::default();
    if spec.is_none() {
        // Preserve whitespace and Markdown source exactly when no range is requested.
        doc.push(text, json!({"kind":kind, "block":1}));
    } else {
        let blocks: Vec<_> = text.split("\n\n").collect();
        for index in selection(spec, blocks.len())? {
            doc.push(blocks[index - 1], json!({"kind":kind, "block":index}));
        }
    }
    Ok(doc)
}

fn table(rows: &[Vec<String>]) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        out.push('|');
        for col in 0..columns {
            out.push(' ');
            let cell = row.get(col).map(String::as_str).unwrap_or("");
            // Markdown's header row already conveys emphasis.
            let cell = if index == 0 {
                cell.strip_prefix("**")
                    .and_then(|s| s.strip_suffix("**"))
                    .unwrap_or(cell)
            } else {
                cell
            };
            out.push_str(&cell.replace('|', "\\|").replace(['\r', '\n'], " "));
            out.push_str(" |");
        }
        out.push('\n');
        if index == 0 {
            out.push_str(&format!("|{}\n", " --- |".repeat(columns)));
        }
    }
    out.trim_end().to_string()
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if matches!(ch, '\\' | '*' | '_' | '[' | ']' | '`' | '<' | '>') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}
