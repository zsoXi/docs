//! docs: one small tool for documents.
//!
//! An AI agent sends a short JSON request and gets a short JSON result. All
//! paths in the request are relative to [`Options::base_dir`] and are validated
//! before use.
//!
//! ```
//! use docs::{run, Input, Options};
//! use std::path::PathBuf;
//!
//! let input = Input::Read {
//!     input: "notes.txt".to_string(),
//!     max_chars: 20000,
//!     locators: false, pages: None, range: None, max_rows: 1000,
//! };
//! let options = Options {
//!     base_dir: PathBuf::from("."),
//!     ..Options::default()
//! };
//! // `run` returns `Result<Output, Error>`; the call is not executed here.
//! let _ = (input, options);
//! ```

mod content;
mod document;
mod docx;
mod docxedit;
mod docxhtml;
mod docxxml;
mod error;
mod fontdecode;
mod htmlpdf;
mod markdown;
mod numbers;
pub mod messages;
mod ocr;
#[doc(hidden)]
pub use ocr::worker as ocr_worker;
mod path;
mod pdf;
mod pdflinks;
mod pdfbudget;
mod pdfdocx;
mod pdfedit;
mod read;
mod schema;
mod request;
pub use request::parse_input;
#[cfg(test)]
mod round17_tests;
#[cfg(test)]
mod round11_tests;
#[cfg(test)]
mod round12_tests;
#[cfg(test)]
mod round13_tests;
#[cfg(test)]
mod round14_tests;
#[cfg(test)]
mod round4_tests;
#[cfg(test)]
mod round5_tests;
#[cfg(test)]
mod round6_tests;
#[cfg(test)]
mod round7_tests;
#[cfg(test)]
mod round8_tests;

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use crate::error::Error;
pub use crate::messages::{MessageCode, Warning};

use crate::messages::{Msg, Params, params};

/// Tool name.
pub const NAME: &str = "docs";

/// One-sentence description of the tool.
pub const DESCRIPTION: &str =
    "One small tool that reads, converts, edits, creates, merges and splits documents.";

/// A request. Tagged by the `op` field.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum Input {
    /// Read a document as Markdown.
    Read {
        /// Input path, relative to `base_dir`.
        input: String,
        /// Maximum number of characters returned.
        #[serde(default = "default_max_chars")]
        max_chars: usize,
        /// Include source-addressable fragments in `data.chunks`.
        #[serde(default)]
        locators: bool,
        /// One-based PDF pages or presentation slides, such as `1-3,5`.
        #[serde(default)]
        pages: Option<String>,
        /// One-based document units, or a spreadsheet range such as `Sheet1!A1:C20`.
        #[serde(default)]
        range: Option<String>,
        /// Maximum rows per worksheet, including the Markdown header row.
        #[serde(default = "default_max_rows")]
        max_rows: usize,
    },
    /// Convert between two formats, chosen from the file extensions.
    Convert {
        /// Input path, relative to `base_dir`.
        input: String,
        /// Output path, relative to `base_dir`.
        output: String,
    },
    /// Replace text in a PDF, DOCX, Markdown, text or HTML file.
    Replace {
        /// Input path, relative to `base_dir`.
        input: String,
        /// Output path, relative to `base_dir`.
        output: String,
        /// Text to find.
        find: String,
        /// Replacement text.
        replace: String,
        /// Replace every occurrence (default) or only the first.
        #[serde(default = "default_true")]
        all: bool,
    },
    /// Create a new DOCX or PDF from Markdown.
    Create {
        /// Markdown source.
        markdown: String,
        /// Output path, relative to `base_dir`.
        output: String,
    },
    /// Merge PDF files.
    Merge {
        /// Input paths, relative to `base_dir`, in order.
        inputs: Vec<String>,
        /// Output path, relative to `base_dir`.
        output: String,
    },
    /// Extract pages from a PDF into a new file.
    Split {
        /// Input path, relative to `base_dir`.
        input: String,
        /// Pages such as `"1-3,5"`.
        pages: String,
        /// Output path, relative to `base_dir`.
        output: String,
    },
}

fn default_max_chars() -> usize {
    20_000
}

fn default_max_rows() -> usize {
    1000
}

fn default_true() -> bool {
    true
}

/// Runtime options.
#[derive(Debug, Clone)]
pub struct Options {
    /// Directory that every input and output path is resolved against.
    pub base_dir: PathBuf,
    /// Maximum accepted size of a single input file.
    pub max_input_bytes: u64,
    /// Font used when creating PDF files. Defaults to a system font.
    pub font_path: Option<PathBuf>,
    /// Trusted directory containing the pinned PP-OCRv5 ONNX models and dictionary.
    /// No downloads. None uses DOCS_OCR_MODELS, or warns when unconfigured.
    pub ocr_models: Option<PathBuf>,
    /// Trusted ONNX Runtime shared library (API 24+); falls back to ORT_DYLIB_PATH.
    pub ocr_runtime: Option<PathBuf>,
    /// Installed docs-ocr helper. Defaults to a sibling of the current executable.
    pub ocr_worker: Option<PathBuf>,
    /// Local OCR workers, clamped to 1..=4. Each uses one inference thread.
    pub ocr_threads: usize,
    /// Hard inference deadline per page, including image decoding and model loading.
    pub ocr_timeout_ms: u64,
    /// Hard per-worker virtual address-space limit, enforced by Linux prlimit.
    pub ocr_max_memory_bytes: u64,
    /// Locale for rendered messages. Only `en` is built in; unknown locales
    /// fall back to `en`.
    pub locale: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            base_dir: PathBuf::from("."),
            max_input_bytes: 100 * 1024 * 1024,
            font_path: None,
            ocr_models: None,
            ocr_runtime: None,
            ocr_worker: None,
            ocr_threads: 2,
            ocr_timeout_ms: 30_000,
            ocr_max_memory_bytes: 2 * 1024 * 1024 * 1024,
            locale: "en".to_string(),
        }
    }
}

impl Options {
    /// Options rooted at `base_dir` with the default limits.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            ..Self::default()
        }
    }
}

/// A result. `None` fields are omitted; `warnings` is always present in JSON.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Output {
    /// Always `true` on success; failures are returned as [`Error`].
    pub ok: bool,
    /// Path of the produced or read file, relative to `base_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Size of the produced or read file in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// One sentence describing what happened, rendered in the locale.
    pub summary: String,
    /// Stable code of the summary message.
    #[serde(default)]
    pub summary_code: String,
    /// Parameters the summary was rendered with.
    #[serde(default)]
    pub summary_params: Params,
    /// Extracted text, for `read` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Source-addressable read fragments when `locators` is requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Non-fatal problems the caller should know about.
    #[serde(default)]
    pub warnings: Vec<Warning>,
}

/// Build an [`Output`], rendering the summary and warnings in the locale.
fn make_output(
    opts: &Options,
    summary: Msg,
    output: Option<String>,
    bytes: Option<u64>,
    text: Option<String>,
    warnings: Vec<Msg>,
) -> Output {
    Output {
        ok: true,
        output,
        bytes,
        summary: summary.render(&opts.locale),
        summary_code: summary.code.as_str().to_string(),
        summary_params: summary.params.clone(),
        text,
        data: None,
        warnings: warnings
            .iter()
            .map(|warning| warning.warning(&opts.locale))
            .collect(),
    }
}

/// JSON Schema of [`Input`].
pub fn schema() -> serde_json::Value {
    schema::build()
}

/// Run one request.
pub fn run(input: &Input, opts: &Options) -> Result<Output, Error> {
    if let Input::Read { max_rows, .. }=input
        && !(1..=100_000).contains(max_rows) {
        return Err(Error::invalid(MessageCode::ReadInvalid,params(&[("field","/max_rows".into()),("minimum",1.into()),("maximum",100_000.into())])));
    }
    // Take every update snapshot under the same locks, including across CLI
    // processes sharing base_dir. Atomic rename lets readers observe complete
    // old or complete new files. Directory locks need no persistent lock file.
    static UPDATES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _update = if matches!(input, Input::Read { .. }) {
        None
    } else {
        let process = UPDATES
            .lock()
            .map_err(|_| Error::invalid(MessageCode::FileUpdateLock, Params::new()))?;
        let directory = std::fs::File::open(&opts.base_dir)
            .map_err(|e| Error::reader(MessageCode::FileUpdateLock, e))?;
        directory
            .lock()
            .map_err(|e| Error::reader(MessageCode::FileUpdateLock, e))?;
        Some((process, directory))
    };
    let result = match input {
        Input::Read {
            input,
            max_chars,
            locators,
            pages,
            range,
            max_rows,
        } => read::run(
            read::Request {
                input,
                max_chars: *max_chars,
                locators: *locators,
                pages: pages.as_deref(),
                range: range.as_deref(),
                max_rows: *max_rows,
            },
            opts,
        ),
        Input::Convert { input, output } => run_convert(input, output, opts),
        Input::Replace {
            input,
            output,
            find,
            replace,
            all,
        } => run_replace(input, output, find, replace, *all, opts),
        Input::Create { markdown, output } => run_create(markdown, output, opts),
        Input::Merge { inputs, output } => run_merge(inputs, output, opts),
        Input::Split {
            input,
            pages,
            output,
        } => run_split(input, pages, output, opts),
    };
    result.map_err(|error| {
        let values=error.params();
        let output_path=match input {
            Input::Read { .. }=>None,
            Input::Convert{output,..}|Input::Replace{output,..}|Input::Create{output,..}|Input::Merge{output,..}|Input::Split{output,..}=>Some(output.as_str()),
        };
        let mut field = match error.code() {
            MessageCode::ReplaceFindEmpty => "/find",
            MessageCode::MergeNoInputs => "/inputs",
            MessageCode::PdfPageRange | MessageCode::ReadRange => if matches!(input, Input::Read { pages: None, .. }) { "/range" } else { "/pages" },
            MessageCode::ConvertOverwritesInput | MessageCode::FileWrite => "/output",
            _ => "/input",
        };
        if values.get("path").and_then(|v|v.as_str()).is_some_and(|p|Some(p)==output_path) { field="/output"; }
        let field=if let Input::Merge{inputs,..}=input {
            values.get("path").and_then(|v|v.as_str()).and_then(|p|inputs.iter().position(|i|i==p)).map(|i|format!("/inputs/{i}")).unwrap_or_else(||field.into())
        } else { field.to_string() };
        if values.get("field").and_then(|v|v.as_str()).is_some_and(|v|v==field || v!="/input") { error }
        else { error.with_params(params(&[("field",field.into())])) }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Pdf,
    Docx,
    Md,
    Txt,
    Html,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Md => "md",
            Self::Txt => "txt",
            Self::Html => "html",
        }
    }
}

fn format_of(path: &Path) -> Result<Format, Error> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => Ok(Format::Pdf),
        "docx" => Ok(Format::Docx),
        "md" | "markdown" => Ok(Format::Md),
        "txt" | "text" => Ok(Format::Txt),
        "html" | "htm" => Ok(Format::Html),
        other => Err(Error::UnsupportedFormat(other.to_string())),
    }
}

fn run_convert(rel_in: &str, rel_out: &str, opts: &Options) -> Result<Output, Error> {
    let in_path = path::resolve(&opts.base_dir, rel_in)?;
    let out_path = path::resolve(&opts.base_dir, rel_out)?;
    let from = format_of(&in_path)?;
    let to = format_of(&out_path)?;
    let aliases = path::same_file(&in_path, &out_path, rel_in, rel_out)?;
    if in_path == out_path || (from == to && aliases) {
        return Err(Error::invalid(
            MessageCode::ConvertOverwritesInput,
            Params::new(),
        ));
    }
    let bytes = path::read_limited(&in_path, rel_in, opts.max_input_bytes)?;
    if bytes.is_empty() { return Err(Error::invalid(MessageCode::InputEmpty, params(&[("path",rel_in.into())]))); }
    if from == Format::Md { markdown::validate(text_input(&bytes)?)?; }
    let font = if to == Format::Pdf {
        pdf::load_font(opts.font_path.as_deref())?
    } else {
        Vec::new()
    };
    let mut warnings = Vec::new();
    if to == Format::Pdf {
        warnings.push(Msg::with(
            MessageCode::ConvertLayoutApproximate,
            params(&[("from", from.name().into()), ("to", to.name().into())]),
        ));
    }

    let mut ocr_data = Vec::new();
    let data = match (from, to) {
        (Format::Pdf, Format::Docx) => {
            let document = pdfbudget::load(&bytes)?;
            let pages: Vec<lopdf::ObjectId> = document.get_pages().into_values().collect();
            let mut scanned = Vec::new();
            for (index, page) in pages.iter().enumerate() {
                // Do not reintroduce forms intentionally omitted by the PDF interpreter.
                if pdfedit::form_warnings(&document, *page)?.is_empty()
                    && pdfedit::positioned_lines(&document, *page)?.iter().all(|l| l.text.trim().is_empty()) {
                    scanned.push(index + 1);
                }
            }
            let mut recognized = std::collections::BTreeMap::new();
            for (index, page) in ocr::pdf_pages(&bytes, &scanned, opts, &mut warnings)? {
                warnings.extend(page.warnings);
                ocr_data.push(ocr::metadata(index, &page.lines));
                recognized.insert(index, page.text);
            }
            let result = pdfdocx::build_with_ocr(&document, &pages, &recognized)?;
            warnings.extend(result.warnings);
            result.data
        }
        (Format::Pdf, Format::Md) | (Format::Pdf, Format::Txt) => {
            warnings.push(Msg::new(MessageCode::ConvertPdfTextLossy));
            let (text, form_warnings) = pdf::extract_text_with_warnings(&bytes, rel_in)?;
            warnings.extend(form_warnings);
            text.into_bytes()
        }
        (Format::Docx, Format::Pdf) => {
            let parsed = docx::parse(&bytes)?;
            let converted = docxhtml::convert(&bytes, &parsed)?;
            let images = converted.pdf_images();
            warnings.extend(converted.warnings);
            let html = markdown::full_html(&converted.body);
            pdf::html_to_pdf_with_families(
                &html,
                &font,
                &images,
                &converted.setup,
                &converted.families,
                &mut warnings,
            )?
        }
        (Format::Docx, Format::Md) | (Format::Docx, Format::Txt) => {
            docx::read_text(&bytes)?.into_bytes()
        }
        (Format::Docx, Format::Docx) => {
            let parsed = docx::parse(&bytes)?;
            let model = docxhtml::convert(&bytes, &parsed)?;
            warnings.extend(model.warnings);
            docx::bound_images(&bytes, &model.setup)?
        }
        (Format::Md, Format::Docx) => {
            let model = document::Document::markdown(text_input(&bytes)?, opts)?;
            let data = docx::blocks_to_docx(&model.blocks()?)?;
            warnings.extend(model.warnings);
            data
        }
        (Format::Md, Format::Pdf) => {
            let text = text_input(&bytes)?;
            let body = markdown::to_html(text);
            let model = document::Document::local(&body, opts, pdf::PageSetup::default())?;
            let images = model.pdf_images();
            warnings.extend(model.warnings);
            pdf::html_to_pdf_with_images(&markdown::full_html(&model.body), &font, &images)?
        }
        (Format::Html, Format::Pdf) => {
            let html = text_input(&bytes)?;
            let model = document::Document::local(html, opts, htmlpdf::page_setup(html))?;
            let images = model.pdf_images();
            warnings.extend(model.warnings);
            htmlpdf::render(&model.body, &font, &images)?
        }
        (Format::Html, Format::Docx) => {
            let html = text_input(&bytes)?;
            let model = document::Document::local(html, opts, pdf::PageSetup::default())?;
            let data = docx::blocks_to_docx(&model.blocks()?)?;
            warnings.extend(model.warnings);
            data
        }
        (Format::Html, Format::Md) => {
            let html = text_input(&bytes)?;
            read::html::markdown(html)?.into_bytes()
        }
        (Format::Txt, Format::Pdf) => {
            let text = text_input(&bytes)?;
            pdf::text_to_pdf(text, &font)?
        }
        (Format::Txt, Format::Docx) => {
            let text = text_input(&bytes)?;
            docx::text_to_docx(text)?
        }
        (from, to) => {
            return Err(Error::UnsupportedConversion {
                from: from.name().to_string(),
                to: to.name().to_string(),
            });
        }
    };

    path::write(&out_path, rel_out, &data)?;
    let mut output = make_output(
        opts,
        Msg::with(
            MessageCode::ConvertDone,
            params(&[
                ("input", rel_in.into()),
                ("from", from.name().into()),
                ("output", rel_out.into()),
                ("to", to.name().into()),
            ]),
        ),
        Some(rel_out.to_string()),
        Some(data.len() as u64),
        None,
        warnings,
    );
    if !ocr_data.is_empty() { output.data = Some(serde_json::json!({"ocr":ocr_data})); }
    Ok(output)
}

fn run_replace(
    rel_in: &str,
    rel_out: &str,
    find: &str,
    replacement: &str,
    all: bool,
    opts: &Options,
) -> Result<Output, Error> {
    if find.is_empty() {
        return Err(Error::invalid(MessageCode::ReplaceFindEmpty, Params::new()));
    }
    let in_path = path::resolve(&opts.base_dir, rel_in)?;
    let out_path = path::resolve(&opts.base_dir, rel_out)?;
    let format = format_of(&in_path)?;
    let bytes = path::read_limited(&in_path, rel_in, opts.max_input_bytes)?;
    let mut warnings = Vec::new();

    let (data, replaced, present) = match format {
        Format::Pdf => {
            let substitutes = pdf::load_substitute_fonts(opts.font_path.as_deref());
            let mut document = pdfbudget::load(&bytes)?;
            let outcome =
                pdfedit::replace_in_document(&mut document, find, replacement, all, &substitutes)?;
            warnings.extend(outcome.warnings);
            let mut data = Vec::new();
            document.save_to(&mut data).map_err(|source| Error::Write {
                path: rel_out.to_string(),
                source,
            })?;
            // Partial glyphs have their own diagnostic, not the historical
            // warning about matches split across text fragments.
            (
                data,
                outcome.replaced,
                outcome.present - outcome.partial_glyphs,
            )
        }
        Format::Docx => docx::replace_text(&bytes, find, replacement, all)?,
        Format::Md | Format::Txt | Format::Html => {
            let text = text_input(&bytes)?.to_owned();
            replace_in_text(text, find, replacement, all)
        }
    };

    if all && replaced < present {
        warnings.push(Msg::with(
            MessageCode::ReplaceSkipped,
            params(&[
                ("skipped", (present - replaced).into()),
                ("total", present.into()),
            ]),
        ));
    }

    path::write(&out_path, rel_out, &data)?;
    Ok(make_output(
        opts,
        Msg::with(
            MessageCode::ReplaceDone,
            params(&[
                ("count", replaced.into()),
                ("find", format!("{find:?}").into()),
                ("path", rel_in.into()),
            ]),
        ),
        Some(rel_out.to_string()),
        Some(data.len() as u64),
        None,
        warnings,
    ))
}

fn run_create(markdown: &str, rel_out: &str, opts: &Options) -> Result<Output, Error> {
    if markdown.len() as u64 > opts.max_input_bytes {
        return Err(Error::TooLarge { path: "markdown".into(), size:markdown.len() as u64, limit:opts.max_input_bytes }.with_params(params(&[("field","/markdown".into())])));
    }
    markdown::validate(markdown).map_err(|e| e.with_params(params(&[("field","/markdown".into())])))?;
    let out_path = path::resolve(&opts.base_dir, rel_out)?;
    let to = format_of(&out_path)?;

    let mut create_warnings = Vec::new();
    let data = match to {
        Format::Docx => {
            let model = document::Document::markdown(markdown, opts)?;
            let data = docx::blocks_to_docx(&model.blocks()?)?;
            create_warnings.extend(model.warnings);
            data
        }
        Format::Pdf => {
            create_warnings.push(Msg::with(
                MessageCode::ConvertLayoutApproximate,
                params(&[("from", "md".into()), ("to", "pdf".into())]),
            ));
            let font = pdf::load_font(opts.font_path.as_deref())?;
            let body = markdown::to_html(markdown);
            let model = document::Document::local(&body, opts, pdf::PageSetup::default())?;
            let images = model.pdf_images();
            create_warnings.extend(model.warnings);
            pdf::html_to_pdf_with_images(&markdown::full_html(&model.body), &font, &images)?
        }
        other => return Err(Error::UnsupportedFormat(other.name().to_string())),
    };

    path::write(&out_path, rel_out, &data)?;
    Ok(make_output(
        opts,
        Msg::with(
            MessageCode::CreateDone,
            params(&[("output", rel_out.into())]),
        ),
        Some(rel_out.to_string()),
        Some(data.len() as u64),
        None,
        create_warnings,
    ))
}

fn run_merge(rel_inputs: &[String], rel_out: &str, opts: &Options) -> Result<Output, Error> {
    if rel_inputs.is_empty() {
        return Err(Error::invalid(MessageCode::MergeNoInputs, Params::new()));
    }
    let out_path = path::resolve(&opts.base_dir, rel_out)?;
    let mut inputs = Vec::with_capacity(rel_inputs.len());
    for rel in rel_inputs {
        let path = path::resolve(&opts.base_dir, rel)?;
        inputs.push(path::read_limited(&path, rel, opts.max_input_bytes)?);
    }
    let mut document = pdf::merge(&inputs)?;
    let mut data = Vec::new();
    document.save_to(&mut data).map_err(|source| Error::Write {
        path: rel_out.into(),
        source,
    })?;
    path::write(&out_path, rel_out, &data)?;
    let bytes = Some(data.len() as u64);

    Ok(make_output(
        opts,
        Msg::with(
            MessageCode::MergeDone,
            params(&[("count", inputs.len().into()), ("output", rel_out.into())]),
        ),
        Some(rel_out.to_string()),
        bytes,
        None,
        Vec::new(),
    ))
}

fn run_split(rel_in: &str, pages: &str, rel_out: &str, opts: &Options) -> Result<Output, Error> {
    let in_path = path::resolve(&opts.base_dir, rel_in)?;
    let out_path = path::resolve(&opts.base_dir, rel_out)?;
    let bytes = path::read_limited(&in_path, rel_in, opts.max_input_bytes)?;
    if bytes.is_empty() {
        return Err(Error::invalid(
            MessageCode::InputEmpty,
            params(&[("path", rel_in.into())]),
        ));
    }

    let mut document = pdfbudget::load(&bytes)?;
    let total = document.get_pages().len() as u32;
    let keep = pdf::parse_pages(pages, total)?;
    let kept = keep.len();
    pdf::keep_pages(&mut document, &keep)?;
    let mut data = Vec::new();
    document.save_to(&mut data).map_err(|source| Error::Write {
        path: rel_out.into(),
        source,
    })?;
    path::write(&out_path, rel_out, &data)?;
    let bytes = Some(data.len() as u64);

    Ok(make_output(
        opts,
        Msg::with(
            MessageCode::SplitDone,
            params(&[
                ("pages", kept.into()),
                ("input", rel_in.into()),
                ("output", rel_out.into()),
            ]),
        ),
        Some(rel_out.to_string()),
        bytes,
        None,
        Vec::new(),
    ))
}

fn text_input(bytes: &[u8]) -> Result<&str, Error> {
    std::str::from_utf8(bytes).map_err(|source| Error::reader(MessageCode::InputUtf8, source))
}

fn replace_in_text(
    text: String,
    find: &str,
    replacement: &str,
    all: bool,
) -> (Vec<u8>, usize, usize) {
    let present = text.matches(find).count();
    let (result, replaced) = if all {
        (text.replace(find, replacement), present)
    } else if let Some(position) = text.find(find) {
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..position]);
        out.push_str(replacement);
        out.push_str(&text[position + find.len()..]);
        (out, 1)
    } else {
        (text, 0)
    };
    (result.into_bytes(), replaced, present)
}

fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

#[cfg(test)]
mod round2_tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "docs-round2-{}-{}-{}",
            std::process::id(),
            unique,
            label
        ));
        let _ = std::fs::create_dir_all(&path);
        path
    }

    fn png() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 40, 40]));
        let mut out = std::io::Cursor::new(Vec::new());
        let _ = image::DynamicImage::ImageRgb8(image).write_to(&mut out, image::ImageFormat::Png);
        out.into_inner()
    }

    #[test]
    fn docx_to_pdf_keeps_table_image_and_heading_size() -> Result<(), Box<dyn std::error::Error>> {
        use docx_rs::{Docx, Paragraph, Pic, Run, Table, TableCell, TableRow};
        let docx = Docx::new()
            .add_paragraph(
                Paragraph::new().add_run(Run::new().add_text("Big Heading").size(36).bold()),
            )
            .add_paragraph(Paragraph::new().add_run(Run::new().add_text("Normal body").size(22)))
            .add_table(Table::new(vec![
                TableRow::new(vec![
                    TableCell::new()
                        .add_paragraph(Paragraph::new().add_run(Run::new().add_text("A"))),
                    TableCell::new()
                        .add_paragraph(Paragraph::new().add_run(Run::new().add_text("B"))),
                ]),
                TableRow::new(vec![
                    TableCell::new()
                        .add_paragraph(Paragraph::new().add_run(Run::new().add_text("1"))),
                    TableCell::new()
                        .add_paragraph(Paragraph::new().add_run(Run::new().add_text("2"))),
                ]),
            ]))
            .add_paragraph(
                Paragraph::new().add_run(Run::new().add_image(Pic::new_with_dimensions(
                    png(),
                    8,
                    8,
                ))),
            );
        let mut cursor = std::io::Cursor::new(Vec::new());
        docx.build().pack(&mut cursor)?;
        let bytes = cursor.into_inner();

        let dir = temp_dir("docxpdf");
        std::fs::write(dir.join("in.docx"), &bytes)?;
        let options = Options::new(dir.clone());
        let result = run(
            &Input::Convert {
                input: "in.docx".to_string(),
                output: "out.pdf".to_string(),
            },
            &options,
        );
        assert!(result.is_ok(), "{result:?}");

        let document = lopdf::Document::load(dir.join("out.pdf"))?;
        let page = document
            .get_pages()
            .into_values()
            .next()
            .ok_or("missing output page")?;
        let (images, lines) = crate::pdfedit::page_graphics(&document, page);
        assert!(images >= 1, "image XObject missing");
        assert!(lines > 4, "table grid lines missing");
        let positioned = crate::pdfedit::positioned_lines(&document, page).unwrap_or_default();
        let heading = positioned
            .iter()
            .find(|line| line.text.contains("Big Heading"))
            .map(|line| line.size)
            .unwrap_or(0.0);
        let body = positioned
            .iter()
            .find(|line| line.text.contains("Normal body"))
            .map(|line| line.size)
            .unwrap_or(0.0);
        assert!(heading > body, "heading {heading} vs body {body}");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    fn column_table_pdf() -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let font_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        }));
        let pixels: Vec<u8> = (0..8 * 8).flat_map(|_| [10u8, 60, 220]).collect();
        let image_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 8,
                "Height" => 8,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8,
            },
            pixels,
        )));

        let mut content = String::new();
        let text = |x: f64, y: f64, value: &str, out: &mut String| {
            out.push_str(&format!(
                "BT /F1 12 Tf 1 0 0 1 {x} {y} Tm ({value}) Tj ET\n"
            ));
        };
        text(50.0, 700.0, "Left one", &mut content);
        text(50.0, 686.0, "Left two", &mut content);
        text(50.0, 672.0, "Left three", &mut content);
        text(330.0, 700.0, "Right one", &mut content);
        text(330.0, 686.0, "Right two", &mut content);
        text(330.0, 672.0, "Right three", &mut content);
        // A bordered table with a grid.
        content.push_str("0 0 0 RG 0.8 w\n");
        for y in [420.0, 400.0, 380.0] {
            content.push_str(&format!("50 {y} m 250 {y} l S\n"));
        }
        for x in [50.0, 150.0, 250.0] {
            content.push_str(&format!("{x} 380 m {x} 420 l S\n"));
        }
        text(70.0, 388.0, "A", &mut content);
        text(170.0, 388.0, "B", &mut content);
        text(70.0, 408.0, "1", &mut content);
        text(170.0, 408.0, "2", &mut content);
        content.push_str("q 8 0 0 8 50 250 cm /Im1 Do Q\n");

        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            content.into_bytes(),
        )));
        let resources_id = doc.add_object(Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            "XObject" => dictionary! { "Im1" => Object::Reference(image_id) },
        }));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => Object::Reference(resources_id),
            "Contents" => Object::Reference(content_id),
        }));
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        }));
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        let _ = doc.save_to(&mut out);
        out
    }

    #[test]
    fn pdf_to_docx_keeps_columns_table_and_image() -> Result<(), Box<dyn std::error::Error>> {
        let bytes = column_table_pdf();
        let dir = temp_dir("pdfdocx");
        std::fs::write(dir.join("in.pdf"), &bytes)?;
        let options = Options::new(dir.clone());
        let result = run(
            &Input::Convert {
                input: "in.pdf".to_string(),
                output: "out.docx".to_string(),
            },
            &options,
        );
        assert!(result.is_ok(), "{result:?}");

        let docx_bytes = std::fs::read(dir.join("out.docx"))?;
        let parsed = docx::parse(&docx_bytes)?;
        assert!(!parsed.images.is_empty(), "image missing from word/media");
        let has_table = parsed
            .document
            .children
            .iter()
            .any(|child| matches!(child, docx_rs::DocumentChild::Table(_)));
        assert!(has_table, "no w:tbl in the output");

        let text = docx::read_text(&docx_bytes).unwrap_or_default();
        let left = text.find("Left one").unwrap_or(usize::MAX);
        let left_last = text.find("Left three").unwrap_or(usize::MAX);
        let right = text.find("Right one").unwrap_or(usize::MAX);
        assert!(left < left_last, "left column out of order");
        assert!(
            left_last < right,
            "columns not read column by column: {text:?}"
        );
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn error_message_is_relative_to_base_dir() {
        let dir = temp_dir("relative");
        let options = Options::new(dir.clone());
        let result = run(
            &Input::Read {
                input: "missing.txt".to_string(),
                max_chars: 10,
                locators: false,
                pages: None,
                range: None,
                max_rows: 1000,
            },
            &options,
        );
        let message = match result {
            Err(error) => error.to_string(),
            Ok(_) => String::new(),
        };
        assert!(message.contains("missing.txt"));
        assert!(!message.contains(dir.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(dir);
    }
    fn reading_order_pdf() -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let regular_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        }));
        let bold_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica-Bold",
            "Encoding" => "WinAnsiEncoding",
        }));

        let mut content = String::new();
        let text = |font: &str, size: u32, x: f64, y: f64, value: &str, out: &mut String| {
            out.push_str(&format!(
                "BT /{font} {size} Tf 1 0 0 1 {x} {y} Tm ({value}) Tj ET\n"
            ));
        };
        // Full-width title and intro, then two columns, the left one holding
        // a bold fragment, and a table under the left column only.
        text("F1", 20, 50.0, 760.0, "Document Title Here", &mut content);
        text(
            "F1",
            12,
            50.0,
            730.0,
            "Intro line that spans the whole width of the page from left to right side",
            &mut content,
        );
        text("F1", 12, 50.0, 690.0, "Left one column text", &mut content);
        text("F1", 12, 50.0, 676.0, "Left two ", &mut content);
        text("F2", 12, 104.0, 676.0, "BoldBit", &mut content);
        text("F1", 12, 50.0, 662.0, "Left three column", &mut content);
        text(
            "F1",
            12,
            330.0,
            690.0,
            "Right one column text",
            &mut content,
        );
        text("F1", 12, 330.0, 676.0, "Right two column", &mut content);
        text("F1", 12, 330.0, 662.0, "Right three column", &mut content);
        content.push_str("0 0 0 RG 0.8 w\n");
        for y in [610.0, 590.0, 570.0] {
            content.push_str(&format!("50 {y} m 200 {y} l S\n"));
        }
        for x in [50.0, 125.0, 200.0] {
            content.push_str(&format!("{x} 570 m {x} 610 l S\n"));
        }
        text("F2", 12, 60.0, 596.0, "CellHead", &mut content);
        text("F2", 12, 135.0, 596.0, "HeadTwo", &mut content);
        text("F1", 12, 60.0, 576.0, "CellData", &mut content);
        text("F1", 12, 135.0, 576.0, "DataTwo", &mut content);

        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            content.into_bytes(),
        )));
        let resources_id = doc.add_object(Object::Dictionary(dictionary! {
            "Font" => dictionary! {
                "F1" => Object::Reference(regular_id),
                "F2" => Object::Reference(bold_id),
            },
        }));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => Object::Reference(resources_id),
            "Contents" => Object::Reference(content_id),
        }));
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        }));
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        let _ = doc.save_to(&mut out);
        out
    }

    #[test]
    fn pdf_to_docx_reading_order_full_width_then_columns_then_table()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = reading_order_pdf();
        let dir = temp_dir("order");
        std::fs::write(dir.join("in.pdf"), &bytes)?;
        let options = Options::new(dir.clone());
        let result = run(
            &Input::Convert {
                input: "in.pdf".to_string(),
                output: "out.docx".to_string(),
            },
            &options,
        );
        assert!(result.is_ok(), "{result:?}");

        let docx_bytes = std::fs::read(dir.join("out.docx"))?;
        let text = docx::read_text(&docx_bytes)?;
        let position_of = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
        let title = position_of("Document Title Here");
        let intro = position_of("Intro line that spans");
        let left_one = position_of("Left one");
        let left_three = position_of("Left three");
        let table_head = position_of("CellHead");
        let table_data = position_of("CellData");
        let right_one = position_of("Right one");
        assert!(title < intro, "title must come first: {text:?}");
        assert!(intro < left_one, "intro before columns: {text:?}");
        assert!(left_one < left_three, "left column in order: {text:?}");
        assert!(
            left_three < table_head,
            "table after the left column: {text:?}"
        );
        assert!(table_head < table_data, "header row first: {text:?}");
        assert!(table_data < right_one, "right column after table: {text:?}");

        // The table is a real w:tbl and the bold fragment stayed bold.
        let parsed = docx::parse(&docx_bytes)?;
        assert!(
            parsed
                .document
                .children
                .iter()
                .any(|child| matches!(child, docx_rs::DocumentChild::Table(_)))
        );
        let mut bold_bit_is_bold = false;
        for child in &parsed.document.children {
            let docx_rs::DocumentChild::Paragraph(paragraph) = child else {
                continue;
            };
            for nested in &paragraph.children {
                let docx_rs::ParagraphChild::Run(run) = nested else {
                    continue;
                };
                let run_text: String = run
                    .children
                    .iter()
                    .filter_map(|item| match item {
                        docx_rs::RunChild::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect();
                if run_text.contains("BoldBit") {
                    bold_bit_is_bold = serde_json::to_value(&run.run_property)
                        .ok()
                        .and_then(|value| value.get("bold").and_then(serde_json::Value::as_bool))
                        .unwrap_or(false);
                }
            }
        }
        assert!(bold_bit_is_bold, "BoldBit run is not bold");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
